//! ytmdl: search YouTube Music and download tagged tracks (Dioxus; desktop and Android).

mod jobs;
mod platform;
mod ui;

use std::time::Instant;

use dioxus::prelude::*;
use ytmdl_core::{Downloader, Runtime};

use jobs::{Queue, Services};
use ui::Boot;

fn main() {
    platform::init_logging();
    dioxus::launch(ui::App);
}

/// Unpacks/locates the runtime and starts Python off the UI thread, then runs the
/// autotest if one was requested.
async fn start(mut boot: Signal<Boot>, queue: Queue) {
    let started = Instant::now();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new().name("ytmdl-start".into()).spawn(move || {
        let result = platform::prepare().and_then(|p| {
            let rt = Runtime::start(p.config)?;
            Ok(Services {
                dl: Downloader::new(rt),
                output_dir: p.output_dir,
                fallback_output_dir: p.fallback_output_dir,
            })
        });
        let _ = tx.send(result);
    });
    let result = match spawned {
        Ok(_) => rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("startup thread panicked"))),
        Err(e) => Err(e.into()),
    };
    let startup_ms = started.elapsed().as_millis();
    let services = match result {
        Ok(s) => {
            tracing::info!(target: "ytmdl", "runtime ready in {startup_ms} ms");
            boot.set(Boot::Ready(s.clone()));
            s
        }
        Err(e) => {
            tracing::error!(target: "ytmdl", "startup failed: {e:#}");
            if platform::autotest_url().is_some() {
                tracing::error!(target: "ytmdl", "YTMDL_E2E fail startup: {e:#}");
            }
            boot.set(Boot::Failed(format!("{e:#}")));
            return;
        }
    };

    if let Some(url) = platform::autotest_url() {
        autotest(services, queue, url, startup_ms).await;
    }
}

/// `autotest`-build end-to-end check: resolve, download through the UI queue, tag,
/// media-scan, then log one `YTMDL_E2E ok|fail <json>` line for the test driver.
async fn autotest(svc: Services, queue: Queue, url: String, startup_ms: u128) {
    tracing::info!(target: "ytmdl", "YTMDL_E2E start {url}");
    let started = Instant::now();
    let result = async {
        let track = match svc.dl.resolve(&url).await? {
            ytmdl_core::Resolved::Track(t) => t,
            ytmdl_core::Resolved::Collection { .. } => anyhow::bail!("autotest URL must be a single track"),
        };
        let resolve_ms = started.elapsed().as_millis();
        let done = queue.run(svc.clone(), jobs::entry_from_track(track)).await?;
        let tags = ytmdl_core::tag::read_tags(&done.path)?;
        let v = svc.dl.runtime().version();
        anyhow::Ok(serde_json::json!({
            "path": done.path,
            "tagged": done.tagged,
            "tags": tags,
            "yt_dlp": v.yt_dlp,
            "python": v.python,
            "startup_ms": startup_ms,
            "resolve_ms": resolve_ms,
            "total_ms": started.elapsed().as_millis(),
        }))
    }
    .await;
    match result {
        Ok(report) => tracing::info!(target: "ytmdl", "YTMDL_E2E ok {report}"),
        Err(e) => tracing::error!(target: "ytmdl", "YTMDL_E2E fail {e:#}"),
    }
}
