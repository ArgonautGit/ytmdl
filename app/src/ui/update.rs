//! yt-dlp updates. YouTube changes often and old yt-dlp builds stop working,
//! so besides the Settings buttons the app checks for a new build once a day
//! (unless that is turned off). A new build is downloaded now and loaded on the
//! next start.

use std::time::{Duration, Instant};

use dioxus::prelude::*;
use ytmdl_core::Channel;
use ytmdl_library::Library;

use super::Ctx;
use super::sync::{ago, unix_now};

const CHECK_EVERY_SECS: i64 = 24 * 3600;
/// A failed automatic check is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(3600);

const AUTO: &str = "ytdlp_auto";
const CHANNEL: &str = "ytdlp_channel";
const CHECKED: &str = "ytdlp_checked_at";

#[derive(Clone, PartialEq, Debug)]
pub struct Updates {
    /// Daily checks are on.
    pub auto: bool,
    /// Stable, or nightly once "Try nightly" was used.
    pub channel: Channel,
    /// Unix seconds of the last check that got an answer.
    pub checked_at: Option<i64>,
    pub checking: bool,
    /// The last check's outcome, for Settings.
    pub message: Option<String>,
    /// The last automatic attempt this run.
    tried: Option<Instant>,
}

impl Updates {
    pub fn load(library: &Library) -> Updates {
        let get = |key| library.setting(key).ok().flatten();
        Updates {
            auto: get(AUTO).as_deref() != Some("0"),
            channel: if get(CHANNEL).as_deref() == Some("nightly") { Channel::Nightly } else { Channel::Stable },
            checked_at: get(CHECKED).and_then(|s| s.parse().ok()),
            checking: false,
            message: None,
            tried: None,
        }
    }

    /// Under the Settings switch.
    pub fn note(&self) -> String {
        if !self.auto {
            return "Off: check with the buttons above".into();
        }
        let channel = match self.channel {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        };
        let checked = self.checked_at.map(|t| format!(" · last checked {}", ago(unix_now() - t)));
        format!("Checks daily for {channel} builds{}", checked.unwrap_or_default())
    }
}

impl Ctx {
    /// Looks for a newer yt-dlp on `channel`, which later automatic checks
    /// follow. `manual` (from Settings) reports even when nothing changed.
    pub(super) fn check_ytdlp(&self, channel: Channel, manual: bool) {
        let Some(svc) = self.services() else { return };
        let mut updates = self.updates;
        if updates.peek().checking {
            return;
        }
        let library = self.library.get();
        if updates.peek().channel != channel {
            let name = if channel == Channel::Nightly { "nightly" } else { "stable" };
            if let Err(e) = library.set_setting(CHANNEL, name) {
                tracing::warn!(target: "ytmdl", "saving the yt-dlp channel: {e}");
            }
        }
        {
            let mut u = updates.write();
            u.channel = channel;
            u.checking = true;
            if manual {
                u.message = Some("Checking…".into());
            } else {
                u.tried = Some(Instant::now());
            }
        }
        let ctx = *self;
        let rt = svc.dl.runtime().clone();
        spawn(async move {
            let result = rt.check_update(channel).await;
            let loaded = rt.version().yt_dlp.clone().unwrap_or_default();
            let message = match &result {
                Ok(o) if o.updated => {
                    if !manual {
                        ctx.notify(format!("Downloaded yt-dlp {}. The app switches to it the next time it starts.", o.version));
                    }
                    format!("Downloaded yt-dlp {}. Restart the app to use it.", o.version)
                }
                Ok(o) if o.version != loaded => format!("yt-dlp {} is downloaded. Restart the app to use it.", o.version),
                Ok(o) => format!("yt-dlp {} is the latest.", o.version),
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "yt-dlp update check: {e}");
                    format!("Update failed: {e}")
                }
            };
            let now = unix_now();
            if result.is_ok()
                && let Err(e) = library.set_setting(CHECKED, &now.to_string())
            {
                tracing::warn!(target: "ytmdl", "saving the update check time: {e}");
            }
            let mut u = updates.write();
            u.checking = false;
            if result.is_ok() {
                u.checked_at = Some(now);
            }
            // A failed automatic check stays quiet; it is tried again later.
            if manual || result.is_ok() {
                u.message = Some(message);
            }
        });
    }

    /// The daily check, when it is due.
    pub(super) fn auto_update_ytdlp(&self) {
        let u = self.updates.peek().clone();
        let due = u.checked_at.is_none_or(|t| unix_now() - t >= CHECK_EVERY_SECS);
        let resting = u.tried.is_some_and(|t| t.elapsed() < RETRY_AFTER);
        if u.auto && due && !resting && !u.checking {
            self.check_ytdlp(u.channel, false);
        }
    }

    pub(super) fn set_auto_update(&self, on: bool) {
        if let Err(e) = self.library.get().set_setting(AUTO, if on { "1" } else { "0" }) {
            self.notify(format!("Couldn't save the setting: {e}"));
            return;
        }
        let mut updates = self.updates;
        updates.write().auto = on;
        if on {
            self.auto_update_ytdlp();
        }
    }
}
