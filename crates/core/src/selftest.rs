//! Offline checks of the native stack, runnable on any target:
//! stdlib native modules, JS challenge solving through yt-dlp + qjs, tagging.
//!
//! Fixtures (recorded on a desktop with `ytmdl record-fixtures`):
//! - `player.js`       a YouTube player script
//! - `challenges.json` [`ChallengeFixture`] with the desktop's answers
//! - `tone.m4a`        a short AAC file to tag

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::TrackMeta;
use crate::runtime::Runtime;
use crate::tag;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeFixture {
    pub player_url: String,
    pub n: Vec<String>,
    pub sig: Vec<String>,
    /// `{"n": {challenge: answer}, "sig": {...}}` as solved on the recording host.
    pub expected: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub name: &'static str,
    pub ok: bool,
    pub millis: u128,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ok: bool,
    pub steps: Vec<Step>,
}

pub async fn run(rt: &Runtime, fixtures: &Path, work: &Path) -> Report {
    let mut steps = Vec::new();

    steps.push(step("version", async { Ok(serde_json::to_value(rt.version())?) }).await);

    steps.push(
        step("stdlib", async {
            let report = rt.check_stdlib().await?;
            let failed: Vec<_> = report
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, v)| !k.starts_with("ssl_") && *k != "openssl" && v.as_str() != Some("ok"))
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            if failed.is_empty() {
                Ok(report)
            } else {
                Err(Error::Invalid(failed.join("; ")))
            }
        })
        .await,
    );

    steps.push(
        step("js_challenges", async {
            let fixture: ChallengeFixture =
                serde_json::from_slice(&std::fs::read(fixtures.join("challenges.json"))?)?;
            let solved = rt
                .solve_challenges(
                    fixture.player_url.clone(),
                    fixtures.join("player.js"),
                    fixture.n.clone(),
                    fixture.sig.clone(),
                )
                .await?;
            let complete = |kind: &str, inputs: &[String]| {
                inputs.iter().all(|c| solved[kind].get(c).and_then(|v| v.as_str()).is_some_and(|v| !v.is_empty()))
            };
            if !complete("n", &fixture.n) || !complete("sig", &fixture.sig) {
                return Err(Error::Invalid(format!("unsolved challenges: {solved}")));
            }
            if let Some(expected) = &fixture.expected
                && *expected != solved
            {
                return Err(Error::Invalid(format!("answers differ from recording host: {solved}")));
            }
            Ok(solved)
        })
        .await,
    );

    steps.push(
        step("tagging", async {
            let src = fixtures.join("tone.m4a");
            std::fs::create_dir_all(work)?;
            let dst: PathBuf = work.join("selftest-tone.m4a");
            std::fs::copy(&src, &dst)?;
            let meta = TrackMeta {
                id: "selftest".into(),
                url: "https://music.youtube.com/watch?v=selftest".into(),
                title: "Self-test tone".into(),
                artists: vec!["ytmdl".into()],
                album: Some("Fixtures".into()),
                album_artists: vec![],
                track_number: Some(1),
                disc_number: None,
                year: Some(2026),
                duration_secs: None,
                cover_urls: vec![],
            };
            let cover = tag::Cover::sniff(std::fs::read(fixtures.join("cover.jpg"))?)
                .ok_or_else(|| Error::Invalid("cover.jpg is not a JPEG".into()))?;
            tag::write_tags(&dst, &meta, Some(cover))?;
            let report = tag::read_tags(&dst)?;
            let ok = report.title.as_deref() == Some("Self-test tone")
                && report.artist.as_deref() == Some("ytmdl")
                && report.year == Some(2026)
                && report.pictures.len() == 1
                && report.duration_secs > 0.5;
            let value = serde_json::to_value(&report)?;
            if ok { Ok(value) } else { Err(Error::Invalid(format!("unexpected tags: {value}"))) }
        })
        .await,
    );

    Report { ok: steps.iter().all(|s| s.ok), steps }
}

async fn step(name: &'static str, f: impl Future<Output = Result<serde_json::Value>>) -> Step {
    let start = Instant::now();
    let result = f.await;
    let millis = start.elapsed().as_millis();
    match result {
        Ok(detail) => Step { name, ok: true, millis, detail },
        Err(e) => Step { name, ok: false, millis, detail: serde_json::Value::String(e.to_string()) },
    }
}
