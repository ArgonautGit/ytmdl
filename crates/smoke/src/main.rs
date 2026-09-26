//! Runs the offline self-test (and optionally one real download) on the target,
//! printing a JSON report. Exit status 0 only if every step passed.
//!
//!   ytmdl-smoke --python-home DIR --ytdlp ZIP --qjs PATH --work DIR --fixtures DIR [--online URL]

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ytmdl_core::{CancelToken, DownloadOptions, Downloader, Runtime, RuntimeConfig, selftest, tag};

struct Args {
    python_home: PathBuf,
    ytdlp: PathBuf,
    qjs: PathBuf,
    work: PathBuf,
    fixtures: PathBuf,
    online: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let (mut python_home, mut ytdlp, mut qjs, mut work, mut fixtures, mut online) =
        (None, None, None, None, None, None);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--python-home" => python_home = Some(PathBuf::from(value()?)),
            "--ytdlp" => ytdlp = Some(PathBuf::from(value()?)),
            "--qjs" => qjs = Some(PathBuf::from(value()?)),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--fixtures" => fixtures = Some(PathBuf::from(value()?)),
            "--online" => online = Some(value()?),
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(Args {
        python_home: python_home.context("--python-home")?,
        ytdlp: ytdlp.context("--ytdlp")?,
        qjs: qjs.context("--qjs")?,
        work: work.context("--work")?,
        fixtures: fixtures.context("--fixtures")?,
        online,
    })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,ytmdl=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = parse_args()?;
    let tokio = tokio::runtime::Builder::new_current_thread().enable_all().build()?;

    let started = std::time::Instant::now();
    let rt = Runtime::start(RuntimeConfig {
        python_home: args.python_home,
        ytdlp_seed: args.ytdlp,
        ytdlp_dir: args.work.join("yt-dlp"),
        qjs: args.qjs,
        cache_dir: args.work.join("cache"),
        tmp_dir: args.work.join("tmp"),
        download_workers: 1,
    })?;
    let startup_ms = started.elapsed().as_millis();

    let (report, online) = tokio.block_on(async {
        let report = selftest::run(&rt, &args.fixtures, &args.work.join("selftest")).await;
        let online = match &args.online {
            None => None,
            Some(url) => Some(online_download(&rt, url, &args.work).await),
        };
        (report, online)
    });

    let online_ok = online.as_ref().is_none_or(|o| o.get("ok") == Some(&serde_json::Value::Bool(true)));
    let out = serde_json::json!({
        "ok": report.ok && online_ok,
        "startup_ms": startup_ms,
        "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "selftest": report,
        "online": online,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    if out["ok"] != true {
        std::process::exit(1);
    }
    Ok(())
}

async fn online_download(rt: &Runtime, url: &str, work: &std::path::Path) -> serde_json::Value {
    let started = std::time::Instant::now();
    let dl = Downloader::new(rt.clone());
    let opts = DownloadOptions::new(work.join("out"));
    match dl.download(url, &opts, |_| {}, CancelToken::new()).await {
        Ok(done) => {
            let tags = tag::read_tags(&done.path).map(|r| serde_json::to_value(r).unwrap_or_default());
            serde_json::json!({
                "ok": done.tagged && tags.is_ok(),
                "millis": started.elapsed().as_millis(),
                "path": done.path,
                "tags": tags.unwrap_or_else(|e| serde_json::Value::String(e.to_string())),
            })
        }
        Err(e) => serde_json::json!({ "ok": false, "millis": started.elapsed().as_millis(), "error": e.to_string() }),
    }
}
