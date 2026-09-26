"""Glue between the Rust host and yt-dlp.

Every value crossing the boundary is a JSON string (or bytes), so the Rust side
only depends on this module's small surface, not on yt-dlp internals. yt-dlp is
imported lazily so the updater can run before a particular yt-dlp build is loaded.
"""

import hashlib
import json
import os
import sys
import tempfile
import urllib.request

_log = None  # host callback: (level: str, message: str) -> None


def _emit(level, msg):
    if _log is not None:
        _log(level, msg)


class _Logger:
    """yt-dlp `logger` param that forwards to the host."""

    def debug(self, msg):
        # yt-dlp routes both debug and info through debug(); info lines lack the prefix.
        _emit("debug" if msg.startswith("[debug] ") else "info", msg)

    def info(self, msg):
        _emit("info", msg)

    def warning(self, msg):
        _emit("warning", msg)

    def error(self, msg):
        _emit("error", msg)


# Android keeps system CAs in these directories, named by OpenSSL's *old* subject
# hash, so OpenSSL 3 can't use them as a CApath. Bundle them into one CAfile instead.
_ANDROID_CA_DIRS = ["/apex/com.android.conscrypt/cacerts", "/system/etc/security/cacerts"]


def _android_ca_bundle(cache_dir):
    seen, pems = set(), []
    for d in _ANDROID_CA_DIRS:
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if name in seen:
                continue
            seen.add(name)
            with open(os.path.join(d, name), encoding="ascii", errors="ignore") as f:
                pems.append(f.read())
    if not pems:
        raise RuntimeError(f"no CA certificates found in {_ANDROID_CA_DIRS}")
    path = os.path.join(cache_dir, "cacerts.pem")
    with open(path + ".part", "w", encoding="ascii") as f:
        f.write("\n".join(pems))
    os.replace(path + ".part", path)
    return path, len(pems)


def configure(config_json, log_callback):
    """Process-wide settings that must be in place before yt-dlp is used."""
    global _log
    _log = log_callback
    cfg = json.loads(config_json)
    os.makedirs(cfg["tmp_dir"], exist_ok=True)
    os.makedirs(cfg["cache_dir"], exist_ok=True)
    tempfile.tempdir = cfg["tmp_dir"]
    report = {"tmp_dir": tempfile.gettempdir()}
    if sys.platform == "android" and "SSL_CERT_FILE" not in os.environ:
        path, count = _android_ca_bundle(cfg["cache_dir"])
        os.environ["SSL_CERT_FILE"] = path
        report["ca_bundle"] = f"{path} ({count} certificates)"
    return json.dumps(report)


def zip_version(zip_path):
    """Reads a yt-dlp zip's version without importing it."""
    import re
    import zipfile

    with zipfile.ZipFile(zip_path) as z:
        src = z.read("yt_dlp/version.py").decode()
    m = re.search(r"""^__version__\s*=\s*['"]([^'"]+)['"]""", src, re.M)
    return m.group(1) if m else None


def load_ytdlp(zip_path):
    """Put a yt-dlp zipimport build on sys.path and import it."""
    if zip_path not in sys.path:
        sys.path.insert(0, zip_path)
    import yt_dlp  # noqa: F401
    return version()


def version():
    import ssl

    info = {
        "python": sys.version.split()[0],
        "platform": sys.platform,
        "openssl": ssl.OPENSSL_VERSION,
    }
    try:
        import yt_dlp.version

        info["yt_dlp"] = yt_dlp.version.__version__
        info["yt_dlp_path"] = os.path.dirname(os.path.dirname(yt_dlp.__file__))
    except ImportError:
        info["yt_dlp"] = None
    return json.dumps(info)


def _ydl(args, extra=None):
    import yt_dlp

    try:
        opts = yt_dlp.parse_options(["--ignore-config", *args]).ydl_opts
    except SystemExit as e:  # optparse reports bad options by exiting
        raise ValueError(f"invalid yt-dlp options: {args!r}") from e
    opts["logger"] = _Logger()
    # The CLI default ("only_download") logs download errors and returns None;
    # raise instead so the host sees the real error. `extra` may override.
    opts["ignoreerrors"] = False
    if extra:
        opts.update(extra)
    return yt_dlp.YoutubeDL(opts)


def _result(ydl, info, url):
    if info is None:
        from yt_dlp.utils import DownloadError

        raise DownloadError(f"yt-dlp returned no result for {url}")
    return json.dumps(ydl.sanitize_info(info))


def extract(args_json, url):
    """Metadata only (no download). Returns sanitized info JSON."""
    with _ydl(json.loads(args_json)) as ydl:
        return _result(ydl, ydl.extract_info(url, download=False), url)


def download(args_json, url, on_progress):
    """Downloads `url`. `on_progress(json) -> bool`; returning False cancels."""
    from yt_dlp.utils import DownloadCancelled

    def hook(d):
        payload = {
            "status": d.get("status"),
            "downloaded_bytes": d.get("downloaded_bytes"),
            "total_bytes": d.get("total_bytes") or d.get("total_bytes_estimate"),
            "speed": d.get("speed"),
            "eta": d.get("eta"),
            "filename": d.get("filename"),
        }
        if not on_progress(json.dumps(payload)):
            raise DownloadCancelled("cancelled by host")

    with _ydl(json.loads(args_json), {"progress_hooks": [hook]}) as ydl:
        return _result(ydl, ydl.extract_info(url, download=True), url)


def fetch(url, max_bytes):
    """Small HTTP GET through yt-dlp's networking stack (e.g. cover art)."""
    import yt_dlp

    with yt_dlp.YoutubeDL({"quiet": True, "logger": _Logger()}) as ydl:
        with ydl.urlopen(url) as resp:
            data = resp.read(max_bytes + 1)
    if len(data) > max_bytes:
        raise ValueError(f"response from {url} exceeds {max_bytes} bytes")
    return data


# --- updater (stdlib only: runs before yt-dlp is imported) ---------------------

_REPOS = {"stable": "yt-dlp/yt-dlp", "nightly": "yt-dlp/yt-dlp-nightly-builds"}


def _get(url, accept=None):
    req = urllib.request.Request(url, headers={"User-Agent": "ytmdl"})
    if accept:
        req.add_header("Accept", accept)
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.read()


def check_update(channel, current_version, dest_dir):
    """Downloads the latest yt-dlp zip for `channel` into `dest_dir` if it is newer.

    The zip is verified against the release's SHA2-256SUMS and written atomically.
    """
    repo = _REPOS[channel]
    release = json.loads(_get(f"https://api.github.com/repos/{repo}/releases/latest",
                              "application/vnd.github+json"))
    tag = release["tag_name"]
    if tag == current_version:
        return json.dumps({"updated": False, "version": tag})

    assets = {a["name"]: a["browser_download_url"] for a in release["assets"]}
    sums = _get(assets["SHA2-256SUMS"]).decode()
    want = next(line.split()[0] for line in sums.splitlines()
                if line.split()[1:] == ["yt-dlp"])
    data = _get(assets["yt-dlp"])
    have = hashlib.sha256(data).hexdigest()
    if have != want:
        raise ValueError(f"yt-dlp {tag}: checksum mismatch ({have} != {want})")

    os.makedirs(dest_dir, exist_ok=True)
    final = os.path.join(dest_dir, f"yt-dlp-{tag}.zip")
    fd, tmp = tempfile.mkstemp(dir=dest_dir, suffix=".part")
    with os.fdopen(fd, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, final)
    return json.dumps({"updated": True, "version": tag, "path": final, "sha256": have})


# --- self-test -----------------------------------------------------------------

def check_stdlib():
    """Imports the native stdlib modules yt-dlp depends on."""
    report = {}
    for name in ["ssl", "_ssl", "_hashlib", "sqlite3", "ctypes", "zlib", "lzma", "bz2",
                 "_json", "_socket", "select", "_posixsubprocess", "_decimal", "pyexpat"]:
        try:
            __import__(name)
            report[name] = "ok"
        except Exception as e:  # report every failure, not just the first
            report[name] = f"{type(e).__name__}: {e}"
    import ssl

    report["openssl"] = ssl.OPENSSL_VERSION
    paths = ssl.get_default_verify_paths()
    report["ssl_capath"] = paths.capath
    report["ssl_cafile"] = paths.cafile
    return json.dumps(report)


def solve_challenges(args_json, player_url, player_path, n_json, sig_json):
    """Solves recorded n/sig challenges offline through yt-dlp's real JS challenge
    director and configured JS runtime (the player JS is pre-seeded, no network)."""
    from yt_dlp.extractor.youtube.jsc._director import initialize_jsc_director
    from yt_dlp.extractor.youtube.jsc.provider import (
        JsChallengeRequest,
        JsChallengeType,
        NChallengeInput,
        SigChallengeInput,
    )

    with open(player_path, encoding="utf-8") as f:
        player = f.read()
    with _ydl(json.loads(args_json)) as ydl:
        ie = ydl.get_info_extractor("Youtube")
        ie._code_cache[ie._player_js_cache_key(player_url)] = player
        director = initialize_jsc_director(ie)
        requests = [
            JsChallengeRequest(JsChallengeType.N, NChallengeInput(player_url, json.loads(n_json))),
            JsChallengeRequest(JsChallengeType.SIG, SigChallengeInput(player_url, json.loads(sig_json))),
        ]
        out = {"n": {}, "sig": {}}
        for request, response in director.bulk_solve(requests):
            key = "n" if request.type is JsChallengeType.N else "sig"
            out[key].update(response.output.results)
        return json.dumps(out)
