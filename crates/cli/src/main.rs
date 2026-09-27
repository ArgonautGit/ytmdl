//! Desktop command line for ytmdl-core (also used to record self-test fixtures).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ytmdl_core::{
    CancelToken, Channel, DownloadOptions, Downloader, Resolved, Runtime, RuntimeConfig, SearchSource, loudness,
    selftest, tag,
};

#[derive(Parser)]
#[command(version, about = "Search and download music from YouTube via embedded yt-dlp")]
struct Cli {
    /// CPython home (contains lib/python3.14). Default: $YTMDL_PYTHON_HOME.
    #[arg(long, global = true)]
    python_home: Option<PathBuf>,
    /// Seed yt-dlp zipimport build. Default: $YTMDL_YTDLP.
    #[arg(long, global = true)]
    ytdlp: Option<PathBuf>,
    /// QuickJS-NG `qjs` executable. Default: $YTMDL_QJS, then PATH.
    #[arg(long, global = true)]
    qjs: Option<PathBuf>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show Python, OpenSSL and yt-dlp versions.
    Version,
    /// Search YouTube Music (or YouTube).
    Search {
        query: String,
        #[arg(short, default_value_t = 10)]
        n: usize,
        #[arg(long, value_enum, default_value_t = Source::Songs)]
        source: Source,
    },
    /// Show a track's metadata or a collection's entries.
    Resolve { url: String },
    /// Download and tag one track.
    Get {
        url: String,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        #[arg(long)]
        no_tags: bool,
        /// Don't measure the loudness for ReplayGain.
        #[arg(long)]
        no_replaygain: bool,
    },
    /// Read back a file's tags and audio properties.
    Inspect { file: PathBuf },
    /// Measure a file's loudness (EBU R128) as ReplayGain.
    Loudness {
        file: PathBuf,
        /// Also store it in the file's ReplayGain tags.
        #[arg(long)]
        write: bool,
    },
    /// Rewrite a fragmented (DASH) M4A as a regular one, in place.
    Remux { file: PathBuf },
    /// Download the latest yt-dlp (takes effect on next start).
    Update {
        #[arg(long, value_enum, default_value_t = UpdateChannel::Stable)]
        channel: UpdateChannel,
    },
    /// Run the offline native-stack checks against recorded fixtures.
    Selftest {
        #[arg(long, default_value = ".deps/fixtures")]
        fixtures: PathBuf,
    },
    /// Record self-test fixtures (needs network and ffmpeg).
    RecordFixtures {
        #[arg(default_value = ".deps/fixtures")]
        dir: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Source {
    Songs,
    Albums,
    Youtube,
}

#[derive(Clone, Copy, ValueEnum)]
enum UpdateChannel {
    Stable,
    Nightly,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,ytmdl=info,symphonia_core::formats::probe=error".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    match &cli.cmd {
        Cmd::Inspect { file } => return print(&cli, &tag::read_tags(file)?, |r| format!("{r:#?}")),
        Cmd::Loudness { file, write } => {
            let gain = loudness::measure(file)?;
            if *write {
                loudness::write(file, gain)?;
            }
            return print(&cli, &gain, |g| {
                format!(
                    "{:.1} LUFS: gain {:+.2} dB, peak {:.6}",
                    loudness::REFERENCE_LUFS - g.gain_db,
                    g.gain_db,
                    g.peak
                )
            });
        }
        Cmd::Remux { file } => {
            let changed = ytmdl_core::remux::defragment_mp4(file)?;
            println!("{}", if changed { "rewritten" } else { "not fragmented; unchanged" });
            return Ok(());
        }
        _ => {}
    }

    let rt = Runtime::start(runtime_config(&cli)?)?;
    let dl = Downloader::new(rt.clone());
    match &cli.cmd {
        Cmd::Version => print(&cli, rt.version(), |v| {
            format!(
                "yt-dlp {} ({})\nPython {} on {}\n{}",
                v.yt_dlp.as_deref().unwrap_or("?"),
                rt.ytdlp_zip().display(),
                v.python,
                v.platform,
                v.openssl
            )
        })?,
        Cmd::Search { query, n, source } => {
            let source = match source {
                Source::Songs => SearchSource::MusicSongs,
                Source::Albums => SearchSource::MusicAlbums,
                Source::Youtube => SearchSource::YouTube,
            };
            let entries = dl.search(query, *n, source).await?;
            print(&cli, &entries, |es| {
                es.iter()
                    .map(|e| format!("{}  {} — {}  {}", e.id, e.artists.join(", "), e.title, e.url))
                    .collect::<Vec<_>>()
                    .join("\n")
            })?
        }
        Cmd::Resolve { url } => print(&cli, &dl.resolve(url).await?, |r| match r {
            Resolved::Track(t) => format!("{} — {} ({:?}, {:?})", t.artists.join(", "), t.title, t.album, t.year),
            Resolved::Collection { title, kind, entries } => {
                let mut s = format!("{kind:?}: {title} ({} entries)", entries.len());
                for e in entries {
                    s.push_str(&format!("\n  {}  {}", e.id, e.title));
                }
                s
            }
        })?,
        Cmd::Get { url, output, no_tags, no_replaygain } => {
            let mut opts = DownloadOptions::new(output);
            opts.write_tags = !no_tags;
            opts.measure_loudness = !no_replaygain;
            let done = dl
                .download(url, &opts, report_progress, CancelToken::new())
                .await
                .inspect_err(|_| eprintln!())?;
            eprintln!();
            print(&cli, &done, |d| format!("{} (tagged: {})", d.path.display(), d.tagged))?
        }
        Cmd::Update { channel } => {
            let channel = match channel {
                UpdateChannel::Stable => Channel::Stable,
                UpdateChannel::Nightly => Channel::Nightly,
            };
            print(&cli, &rt.check_update(channel).await?, |o| {
                if o.updated {
                    format!("downloaded yt-dlp {}; active on next start", o.version)
                } else {
                    format!("yt-dlp {} is current", o.version)
                }
            })?
        }
        Cmd::Selftest { fixtures } => {
            let work = rt.config().tmp_dir.join("selftest");
            let report = selftest::run(&rt, fixtures, &work).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.ok {
                bail!("self-test failed");
            }
        }
        Cmd::RecordFixtures { dir } => record_fixtures(&rt, dir).await?,
        Cmd::Inspect { .. } | Cmd::Remux { .. } | Cmd::Loudness { .. } => unreachable!(),
    }
    Ok(())
}

fn print<T: serde::Serialize>(cli: &Cli, value: &T, text: impl Fn(&T) -> String) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", text(value));
    }
    Ok(())
}

fn report_progress(p: ytmdl_core::Progress) {
    let mb = |b: Option<u64>| b.map(|b| format!("{:.1}", b as f64 / 1e6)).unwrap_or_else(|| "?".into());
    let pct = p.fraction().map(|f| format!("{:5.1}%", f * 100.0)).unwrap_or_else(|| "    ?%".into());
    let speed = p.speed.map(|s| format!("{:.1} MB/s", s / 1e6)).unwrap_or_default();
    eprint!("\r{pct}  {}/{} MB  {speed}      ", mb(p.downloaded_bytes), mb(p.total_bytes));
    let _ = std::io::stderr().flush();
}

fn runtime_config(cli: &Cli) -> Result<RuntimeConfig> {
    let from_env = |flag: &Option<PathBuf>, var: &str| -> Option<PathBuf> {
        flag.clone().or_else(|| std::env::var_os(var).map(PathBuf::from))
    };
    let python_home = from_env(&cli.python_home, "YTMDL_PYTHON_HOME")
        .context("set --python-home or YTMDL_PYTHON_HOME (the nix devshell sets it)")?;
    let ytdlp_seed =
        from_env(&cli.ytdlp, "YTMDL_YTDLP").context("set --ytdlp or YTMDL_YTDLP (run tools/fetch-deps)")?;
    let qjs = from_env(&cli.qjs, "YTMDL_QJS")
        .or_else(|| which("qjs"))
        .context("set --qjs or YTMDL_QJS, or put qjs on PATH")?;
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    let xdg = |var: &str, fallback: &str| {
        std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| home.join(fallback)).join("ytmdl")
    };
    let data = xdg("XDG_DATA_HOME", ".local/share");
    let cache = xdg("XDG_CACHE_HOME", ".cache");
    Ok(RuntimeConfig {
        python_home,
        ytdlp_seed,
        ytdlp_dir: data.join("yt-dlp"),
        qjs,
        tmp_dir: cache.join("tmp"),
        cache_dir: cache,
        download_workers: 2,
    })
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Records what `selftest` needs: a current player script, synthetic challenges
/// with this host's answers, and small generated media files.
async fn record_fixtures(rt: &Runtime, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let api = rt.fetch("https://www.youtube.com/iframe_api".into(), 1 << 20).await?;
    let api = String::from_utf8_lossy(&api).replace("\\/", "/");
    let player_id: String = api
        .split("/s/player/")
        .nth(1)
        .context("no player URL in iframe_api")?
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if player_id.len() < 8 {
        bail!("unexpected player id {player_id:?}");
    }
    let player_url = format!("https://www.youtube.com/s/player/{player_id}/player_ias.vflset/en_US/base.js");
    let player = rt.fetch(player_url.clone(), 16 << 20).await?;
    std::fs::write(dir.join("player.js"), &player)?;
    eprintln!("player {player_id}: {} bytes", player.len());

    let n = vec!["ZdZIqFPQK-Ty8wId".to_string(), "0eRGgQWJGfT5rFHFj".to_string()];
    let sig: String = (0..108).map(|i| b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"[(i * 7 + 3) % 64] as char).collect();
    let sig = vec![sig];
    let solved = rt.solve_challenges(player_url.clone(), dir.join("player.js"), n.clone(), sig.clone()).await?;
    for (kind, inputs) in [("n", &n), ("sig", &sig)] {
        for c in inputs {
            if solved[kind].get(c).and_then(|v| v.as_str()).is_none_or(str::is_empty) {
                bail!("{kind} challenge {c:?} was not solved (is qjs detected? try RUST_LOG=yt_dlp=debug): {solved}");
            }
        }
    }
    let fixture = selftest::ChallengeFixture { player_url, n, sig, expected: Some(solved) };
    std::fs::write(dir.join("challenges.json"), serde_json::to_vec_pretty(&fixture)?)?;

    ffmpeg(&["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:a", "aac", "-b:a", "64k"], &dir.join("tone.m4a"))?;
    ffmpeg(&["-f", "lavfi", "-i", "color=c=0x3355aa:s=64x64", "-frames:v", "1"], &dir.join("cover.jpg"))?;
    println!("{}", serde_json::to_string_pretty(&fixture)?);
    Ok(())
}

fn ffmpeg(args: &[&str], out: &Path) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .arg(out)
        .status()
        .context("running ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg failed for {}", out.display());
    }
    Ok(())
}
