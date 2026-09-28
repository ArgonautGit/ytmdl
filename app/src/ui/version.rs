//! Which build this is, for Settings → About, and "Updated to build <n>" on
//! the first start of a newer build than the one that ran before.

use dioxus::prelude::*;
use ytmdl_library::Library;

use super::Ctx;

/// Written by builds that checked GitHub releases for updates of themselves.
const CHECKED: &str = "app_update_checked_at";
/// The build that ran last, to notice that an update was installed.
const LAST_BUILD: &str = "app_last_build";

/// This build's number (tools/build-number), which tools/build-apk compiles in.
pub fn this_build() -> Option<u32> {
    option_env!("YTMDL_BUILD")?.parse().ok()
}

/// "0.1.0 • build 57 • 1a2b3c4", as far as this build knows, for About.
pub fn version_text() -> String {
    let mut parts = vec![env!("CARGO_PKG_VERSION").to_owned()];
    if let Some(build) = this_build() {
        parts.push(format!("build {build}"));
    }
    if let Some(commit) = option_env!("YTMDL_COMMIT") {
        parts.push(short_commit(commit));
    }
    parts.join(" • ")
}

fn short_commit(commit: &str) -> String {
    let (sha, dirty) = match commit.strip_suffix("-dirty") {
        Some(sha) => (sha, true),
        None => (commit, false),
    };
    let short = sha.get(..7).unwrap_or(sha);
    if dirty { format!("{short}, modified") } else { short.to_owned() }
}

/// What to say on start: that an update was installed, on the first start of
/// a build newer than the one that ran before (`last`; `None` when none did).
/// A first install says nothing.
fn updated_message(last: Option<u32>, current: u32) -> Option<String> {
    (last? < current).then(|| format!("Updated to build {current}"))
}

/// The build that ran before this start, if one did. Builds from before the
/// record didn't write it; one that checked for updates of itself was such a
/// build, so it counts as older than any.
fn last_build(library: &Library) -> Option<u32> {
    let get = |key| library.setting(key).ok().flatten();
    get(LAST_BUILD).and_then(|s| s.parse().ok()).or(get(CHECKED).map(|_| 0))
}

/// Records this build as the one that ran, and says so when it is an update.
pub(super) fn use_updated_notice(ctx: Ctx) {
    use_hook(move || {
        let Some(current) = this_build() else { return };
        let library = ctx.library.get();
        let last = last_build(&library);
        if last == Some(current) {
            return;
        }
        if let Err(e) = library.set_setting(LAST_BUILD, &current.to_string()) {
            tracing::warn!(target: "ytmdl", "saving the build that ran: {e}");
        }
        if let Some(message) = updated_message(last, current) {
            ctx.notify(message);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_an_update_was_installed() {
        assert_eq!(updated_message(Some(14), 15).as_deref(), Some("Updated to build 15"));
        // A first install, the same build again, or an older one installed by hand.
        assert_eq!(updated_message(None, 15), None);
        assert_eq!(updated_message(Some(15), 15), None);
        assert_eq!(updated_message(Some(16), 15), None);
    }

    #[test]
    fn remembers_the_build_that_ran() {
        let art = std::env::temp_dir().join(format!("ytmdl-version-art-{}", std::process::id()));
        let library = Library::open_in_memory(&art).unwrap();
        assert_eq!(last_build(&library), None);
        // An older build with the updater ran: it checked, but didn't record itself.
        library.set_setting(CHECKED, "1790000000").unwrap();
        assert_eq!(last_build(&library), Some(0));
        library.set_setting(LAST_BUILD, "15").unwrap();
        assert_eq!(last_build(&library), Some(15));
    }

    #[test]
    fn describes_this_build() {
        assert_eq!(short_commit("0123456789abcdef"), "0123456");
        assert_eq!(short_commit("0123456789abcdef-dirty"), "0123456, modified");
        assert_eq!(short_commit("abc"), "abc");
        assert!(version_text().starts_with(env!("CARGO_PKG_VERSION")));
    }
}
