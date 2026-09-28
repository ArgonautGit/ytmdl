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
        # The host rewrites DASH m4a itself (remux.rs), so this advice doesn't apply.
        _emit("debug" if "writing DASH m4a" in msg else "warning", msg)

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
        _install_music_metadata(ydl)
        return _result(ydl, ydl.extract_info(url, download=False), url)


# yt-dlp reduces YouTube Music search results to id + title (albums to a bare
# browse id). The rows YouTube sends also carry artists, album, duration, year and
# square art, which is what a results list needs; add them to yt-dlp's entries.
# yt-dlp still makes every request, and when YouTube changes the row layout only
# these extras are lost.
_MUSIC_HOOKS = ("_music_reponsive_list_entry", "_music_responsive_list_entry")
_music_hooked = False
_KINDS = {"song", "video", "album", "ep", "single", "playlist", "episode", "podcast", "artist", "profile"}


def _install_music_metadata(ydl):
    global _music_hooked
    if _music_hooked:
        return
    _music_hooked = True
    try:
        ie = ydl.get_info_extractor("YoutubeMusicSearchURL")
        owner, name = next(
            (c, n) for c in type(ie).__mro__ for n in _MUSIC_HOOKS if n in vars(c)
        )
    except Exception as e:  # layout of yt-dlp's extractors changed
        _emit("warning", f"music search rows stay minimal: {e!r}")
        return
    original = getattr(owner, name)

    def entry(self, renderer):
        result = original(self, renderer)
        if isinstance(result, dict):
            try:
                for key, value in _music_row(renderer).items():
                    if result.get(key) is None:
                        result[key] = value
            except Exception as e:
                _emit("debug", f"music row not understood: {e!r}")
        return result

    setattr(owner, name, entry)


def _music_row(renderer):
    """Fields of one musicResponsiveListItemRenderer (a search result row)."""
    from yt_dlp.utils import traverse_obj

    columns = traverse_obj(renderer, (
        "flexColumns", ..., "musicResponsiveListItemFlexColumnRenderer", "text", "runs"))
    if not columns:
        return {}
    # The columns after the title read "Artist & Artist • Album • 3:12 • 1M plays".
    row = _described([run for column in columns[1:] for run in [*column, {"text": " • "}]])
    row["title"] = "".join(r.get("text", "") for r in columns[0]).strip() or None
    thumbnails = _thumbnails(traverse_obj(renderer, (
        "thumbnail", "musicThumbnailRenderer", "thumbnail", "thumbnails")))
    if thumbnails:
        row["thumbnails"] = thumbnails
    return row


def _page_type(run):
    from yt_dlp.utils import traverse_obj

    return traverse_obj(run, ("navigationEndpoint", "browseEndpoint",
                              "browseEndpointContextSupportedConfigs",
                              "browseEndpointContextMusicConfig", "pageType"))


def _described(runs):
    """Artists, album, length, year and kind from runs reading like
    "Artist & Artist • Album • 3:12 • 1M plays" (split at the " • ")."""
    import re

    from yt_dlp.utils import parse_duration

    groups, group = [], []
    for run in [*runs, {"text": " • "}]:
        if run.get("text", "").strip() == "•":
            groups.append(group)
            group = []
        else:
            group.append(run)

    row, artists = {}, []
    for group in groups:
        text = "".join(r.get("text", "") for r in group).strip()
        types = {_page_type(r) for r in group}
        if not text:
            continue
        if "MUSIC_PAGE_TYPE_ALBUM" in types:
            row["album"] = text
        elif types & {"MUSIC_PAGE_TYPE_ARTIST", "MUSIC_PAGE_TYPE_USER_CHANNEL"}:
            # A song's own channel is "<Artist> - Topic".
            artists += [t.removesuffix(" - Topic") for t in (r.get("text", "").strip() for r in group)
                        if t not in ("", "&", ",")]
        elif re.fullmatch(r"\d{1,2}(:\d{2}){1,2}", text):
            row["duration"] = parse_duration(text)
        elif re.fullmatch(r"\d{4}", text):
            row["release_year"] = int(text)
        elif text.lower() in _KINDS and "ytmdl_kind" not in row:
            row["ytmdl_kind"] = text.lower()
        elif not artists and not re.search(r"\d.*\b(plays|views|likes|songs|subscribers|audience|listeners)$", text):
            artists = [a.removesuffix(" - Topic") for a in re.split(r", | & ", text) if a]
    if artists:
        row["artists"] = artists
    return row


def _thumbnails(thumbnails):
    return [{"url": t["url"], "width": t.get("width"), "height": t.get("height")}
            for t in thumbnails or [] if isinstance(t, dict) and t.get("url")]


def _text(value):
    """The text of a {"runs": [...]} or {"simpleText": ...} value."""
    if not isinstance(value, dict):
        return None
    if "simpleText" in value:
        return value["simpleText"] or None
    return "".join(r.get("text", "") for r in value.get("runs") or []).strip() or None


# --- YouTube Music pages yt-dlp has no extractor for ---------------------------
#
# A song's radio (the songs YouTube Music plays after it), an artist's mix, and
# artist pages. yt-dlp still makes every request (client, headers, retries);
# only the reading of YouTube Music's answers is ours, so when their layout
# changes these pages break and nothing else does.

_MUSIC_VIDEO_SONG = "MUSIC_VIDEO_TYPE_ATV"  # a song's audio, as opposed to a video
_RADIO_PAGES = 5


def _music_api(ydl, ep, query, item_id, key):
    ie = ydl.get_info_extractor("YoutubeTab")
    return ie._extract_response(item_id=item_id, query=query, ep=ep, default_client="web_music",
                                check_get_keys=key)


def _watch_url(video_id):
    return f"https://music.youtube.com/watch?v={video_id}"


def _video_kind(watch_endpoint):
    from yt_dlp.utils import traverse_obj

    kind = traverse_obj(watch_endpoint, (
        "watchEndpointMusicSupportedConfigs", "watchEndpointMusicConfig", "musicVideoType"))
    return "song" if kind == _MUSIC_VIDEO_SONG else "video"


def _queue_row(item):
    """A song of a radio (playlistPanelVideoRenderer)."""
    from yt_dlp.utils import parse_duration, traverse_obj

    # A song with a music video comes wrapped; the primary is what plays.
    r = traverse_obj(item, (
        ("playlistPanelVideoRenderer", ("playlistPanelVideoWrapperRenderer", "primaryRenderer",
                                        "playlistPanelVideoRenderer")), {dict}), get_all=False)
    if not r or not r.get("videoId"):
        return None
    row = _described(traverse_obj(r, ("longBylineText", "runs")) or [])
    row.update({
        "_type": "url",
        "id": r["videoId"],
        "url": _watch_url(r["videoId"]),
        "title": _text(r.get("title")),
        "ytmdl_kind": _video_kind(traverse_obj(r, ("navigationEndpoint", "watchEndpoint"))),
    })
    duration = parse_duration(_text(r.get("lengthText")))
    if duration:
        row["duration"] = duration
    thumbnails = _thumbnails(traverse_obj(r, ("thumbnail", "thumbnails")))
    if thumbnails:
        row["thumbnails"] = thumbnails
    return row


def music_radio(args_json, request_json):
    """Songs to play after one song (`video_id`), or the mix `playlist_id`
    names (an artist's), up to `limit`. Returns {"entries": [...]}."""
    from yt_dlp.utils import traverse_obj

    request = json.loads(request_json)
    video_id, limit = request.get("video_id"), request["limit"]
    query = {"enablePersistentPlaylistPanel": True, "isAudioOnly": True,
             "tunerSettingValue": "AUTOMIX_SETTING_NORMAL"}
    if video_id:
        query.update(videoId=video_id, playlistId=request.get("playlist_id") or f"RDAMVM{video_id}",
                     params=request.get("params") or "wAEB")
    else:
        query["playlistId"] = request["playlist_id"]
        if request.get("params"):
            query["params"] = request["params"]
    entries, seen = [], set()
    with _ydl(json.loads(args_json)) as ydl:
        data = _music_api(ydl, "next", query, query["playlistId"], "contents")
        panel = traverse_obj(data, (
            "contents", "singleColumnMusicWatchNextResultsRenderer", "tabbedRenderer",
            "watchNextTabbedResultsRenderer", "tabs", 0, "tabRenderer", "content",
            "musicQueueRenderer", "content", "playlistPanelRenderer", {dict})) or {}
        for page in range(_RADIO_PAGES):
            for item in panel.get("contents") or []:
                row = _queue_row(item)
                if row and row["id"] not in seen:
                    seen.add(row["id"])
                    entries.append(row)
            token = traverse_obj(panel, (
                "continuations", 0, ("nextRadioContinuationData", "nextContinuationData"),
                "continuation", {str}), get_all=False)
            if len(entries) >= limit or not token:
                break
            data = _music_api(ydl, "next", {"continuation": token},
                              f"{query['playlistId']} page {page + 2}", "continuationContents")
            panel = traverse_obj(data, ("continuationContents", "playlistPanelContinuation", {dict})) or {}
    return json.dumps({"entries": entries[:limit]})


def _browse_target(endpoint):
    """Where a "More" button leads: {"browse_id", "params"}."""
    if not isinstance(endpoint, dict) or not endpoint.get("browseId"):
        return None
    return {"browse_id": endpoint["browseId"], "params": endpoint.get("params")}


def _list_item(renderer):
    """A song row of an artist page or list (musicResponsiveListItemRenderer)."""
    from yt_dlp.utils import traverse_obj

    if not isinstance(renderer, dict):
        return None
    video_id = traverse_obj(renderer, ("playlistItemData", "videoId", {str})) or traverse_obj(
        renderer, ("overlay", ..., "watchEndpoint", "videoId", {str}), get_all=False)
    if video_id:
        row = _music_row(renderer)
        row.update({"_type": "url", "id": video_id, "url": _watch_url(video_id)})
        row.setdefault("ytmdl_kind", "song")
        return row
    # An album, playlist or artist listed as a row.
    return _browse_item(
        _music_row(renderer), traverse_obj(renderer, ("navigationEndpoint", "browseEndpoint")))


def _browse_item(row, endpoint):
    """`row` as the album, playlist or artist `endpoint` opens, else None."""
    from yt_dlp.utils import traverse_obj

    browse_id = traverse_obj(endpoint, ("browseId", {str}))
    page = traverse_obj(endpoint, (
        "browseEndpointContextSupportedConfigs", "browseEndpointContextMusicConfig", "pageType"))
    if not browse_id or not row.get("title"):
        return None
    if page == "MUSIC_PAGE_TYPE_ALBUM":
        row.update(id=browse_id, url=f"https://music.youtube.com/browse/{browse_id}")
        if row.get("ytmdl_kind") not in ("album", "ep", "single"):
            row["ytmdl_kind"] = None
    elif page == "MUSIC_PAGE_TYPE_PLAYLIST":
        playlist_id = browse_id[2:] if browse_id.startswith("VL") else browse_id
        row.update(id=playlist_id, url=f"https://music.youtube.com/playlist?list={playlist_id}",
                   ytmdl_kind="playlist")
    elif page in ("MUSIC_PAGE_TYPE_ARTIST", "MUSIC_PAGE_TYPE_USER_CHANNEL"):
        # Their subtitle is the audience, not artists.
        row.update(id=browse_id, url=f"https://music.youtube.com/browse/{browse_id}",
                   ytmdl_kind="artist", artists=[])
    else:  # podcasts, episodes
        return None
    row["_type"] = "url"
    row["ytmdl_page"] = {"MUSIC_PAGE_TYPE_USER_CHANNEL": "artist"}.get(page) or page[len("MUSIC_PAGE_TYPE_"):].lower()
    return row


def _tile(item):
    """An item of a carousel or grid (musicTwoRowItemRenderer)."""
    from yt_dlp.utils import traverse_obj

    r = item.get("musicTwoRowItemRenderer")
    if not isinstance(r, dict):
        return _list_item(item.get("musicResponsiveListItemRenderer"))
    row = _described(traverse_obj(r, ("subtitle", "runs")) or [])
    row["title"] = _text(r.get("title"))
    thumbnails = _thumbnails(traverse_obj(r, (
        "thumbnailRenderer", "musicThumbnailRenderer", "thumbnail", "thumbnails")))
    if thumbnails:
        row["thumbnails"] = thumbnails
    watch = traverse_obj(r, ("navigationEndpoint", "watchEndpoint", {dict}))
    if watch and watch.get("videoId"):
        row.update({"_type": "url", "id": watch["videoId"], "url": _watch_url(watch["videoId"]),
                    "ytmdl_kind": _video_kind(watch)})
        return row if row["title"] else None
    return _browse_item(row, traverse_obj(r, ("navigationEndpoint", "browseEndpoint")))


# What an artist page's section lists, from its items.
_SECTION_KINDS = {"song": "songs", "video": "videos", "album": "albums", "playlist": "playlists",
                  "artist": "artists"}


def _section(title, entries, more):
    def kind(e):
        return e.get("ytmdl_page") or e.get("ytmdl_kind")

    entries = [e for e in entries if e and _SECTION_KINDS.get(kind(e))]
    if not entries:
        return None
    first = kind(entries[0])
    return {"title": title, "kind": _SECTION_KINDS[first], "more": more,
            "entries": [e for e in entries if kind(e) == first]}


def music_artist(args_json, request_json):
    """A YouTube Music artist page (`browse_id`, the channel id): the artist,
    their mix, and the sections the page lists (top songs, albums, singles,
    videos, playlists, similar artists)."""
    from yt_dlp.utils import traverse_obj

    browse_id = json.loads(request_json)["browse_id"]
    with _ydl(json.loads(args_json)) as ydl:
        data = _music_api(ydl, "browse", {"browseId": browse_id}, browse_id, "contents")
    header = traverse_obj(data, ("header", ("musicImmersiveHeaderRenderer", "musicVisualHeaderRenderer"),
                                 {dict}), get_all=False) or {}
    radio = traverse_obj(header, ("startRadioButton", "buttonRenderer", "navigationEndpoint",
                                  "watchPlaylistEndpoint", {dict}))
    page = {
        "id": browse_id,
        "name": _text(header.get("title")),
        "thumbnails": _thumbnails(traverse_obj(header, (
            ("foregroundThumbnail", "thumbnail"), "musicThumbnailRenderer", "thumbnail", "thumbnails"),
            get_all=False)),
        "subscribers": _text(traverse_obj(header, ("subscriptionButton", "subscribeButtonRenderer",
                                                   "subscriberCountText"))),
        "audience": _text(header.get("monthlyListenerCount")),
        "radio": {"playlist_id": radio["playlistId"], "params": radio.get("params")}
        if radio and radio.get("playlistId") else None,
        "description": None,
        "sections": [],
    }
    for content in traverse_obj(data, ("contents", "singleColumnBrowseResultsRenderer", "tabs", 0,
                                       "tabRenderer", "content", "sectionListRenderer", "contents",
                                       ..., {dict})):
        section = None
        if shelf := content.get("musicShelfRenderer"):
            section = _section(
                _text(shelf.get("title")),
                [_list_item(i.get("musicResponsiveListItemRenderer")) for i in shelf.get("contents") or []],
                _browse_target(traverse_obj(shelf, ("bottomEndpoint", "browseEndpoint")))
                or _browse_target(traverse_obj(shelf, ("title", "runs", 0, "navigationEndpoint", "browseEndpoint"))))
        elif shelf := content.get("musicCarouselShelfRenderer"):
            head = traverse_obj(shelf, ("header", "musicCarouselShelfBasicHeaderRenderer", {dict})) or {}
            section = _section(
                _text(head.get("title")),
                [_tile(i) for i in shelf.get("contents") or [] if isinstance(i, dict)],
                _browse_target(traverse_obj(head, ("moreContentButton", "buttonRenderer",
                                                   "navigationEndpoint", "browseEndpoint")))
                or _browse_target(traverse_obj(head, ("title", "runs", 0, "navigationEndpoint", "browseEndpoint"))))
        elif shelf := content.get("musicDescriptionShelfRenderer"):
            page["description"] = _text(shelf.get("description"))
        if section:
            page["sections"].append(section)
    return json.dumps(page)


def music_browse(args_json, request_json):
    """Everything a "More" button of an artist page lists (`browse_id` and
    `params`: all their albums, say), up to `limit`. Returns {"entries": [...]}."""
    from yt_dlp.utils import traverse_obj

    request = json.loads(request_json)
    browse_id, limit = request["browse_id"], request["limit"]
    query = {"browseId": browse_id}
    if request.get("params"):
        query["params"] = request["params"]
    entries, seen = [], set()

    def add(items, parse):
        for item in items or []:
            row = parse(item) if isinstance(item, dict) else None
            if row and row["id"] not in seen:
                seen.add(row["id"])
                entries.append(row)

    with _ydl(json.loads(args_json)) as ydl:
        data = _music_api(ydl, "browse", query, browse_id, "contents")
        token = None
        for content in traverse_obj(data, ("contents", "singleColumnBrowseResultsRenderer", "tabs", 0,
                                           "tabRenderer", "content", "sectionListRenderer", "contents",
                                           ..., {dict})):
            for key, items, parse in (("gridRenderer", "items", _tile),
                                      ("musicShelfRenderer", "contents",
                                       lambda i: _list_item(i.get("musicResponsiveListItemRenderer"))),
                                      ("musicCarouselShelfRenderer", "contents", _tile)):
                if shelf := content.get(key):
                    add(shelf.get(items), parse)
                    token = token or traverse_obj(shelf, ("continuations", 0, "nextContinuationData",
                                                          "continuation", {str}))
        for page in range(_RADIO_PAGES):
            if len(entries) >= limit or not token:
                break
            data = _music_api(ydl, "browse", {"continuation": token}, f"{browse_id} page {page + 2}",
                              "continuationContents")
            more = traverse_obj(data, ("continuationContents", ("gridContinuation", "musicShelfContinuation"),
                                       {dict}), get_all=False) or {}
            add(more.get("items"), _tile)
            add(more.get("contents"), lambda i: _list_item(i.get("musicResponsiveListItemRenderer")))
            token = traverse_obj(more, ("continuations", 0, "nextContinuationData", "continuation", {str}))
    return json.dumps({"entries": entries[:limit]})


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


def fetch(url, max_bytes, headers_json="{}", missing_ok=False):
    """Small HTTP GET through yt-dlp's networking stack (e.g. cover art).
    With `missing_ok`, a 404 answer returns None instead of raising."""
    import yt_dlp
    from yt_dlp.networking import Request
    from yt_dlp.networking.exceptions import HTTPError

    request = Request(url, headers=json.loads(headers_json))
    with yt_dlp.YoutubeDL({"quiet": True, "logger": _Logger()}) as ydl:
        try:
            with ydl.urlopen(request) as resp:
                data = resp.read(max_bytes + 1)
        except HTTPError as e:
            if missing_ok and e.status == 404:
                return None
            raise
    if len(data) > max_bytes:
        raise ValueError(f"response from {url} exceeds {max_bytes} bytes")
    return data


def fetch_to_file(url, path, max_bytes):
    """Streams a large HTTP GET (an app update) to `path`, reading at most
    `max_bytes`; returns the SHA-256 of what it wrote, in hex."""
    import yt_dlp
    from yt_dlp.networking import Request

    digest, size = hashlib.sha256(), 0
    with yt_dlp.YoutubeDL({"quiet": True, "logger": _Logger()}) as ydl:
        with ydl.urlopen(Request(url)) as resp, open(path, "wb") as f:
            while chunk := resp.read(1 << 16):
                size += len(chunk)
                if size > max_bytes:
                    raise ValueError(f"response from {url} exceeds {max_bytes} bytes")
                digest.update(chunk)
                f.write(chunk)
    return digest.hexdigest()


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
