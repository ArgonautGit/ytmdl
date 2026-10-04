use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The embedded interpreter failed to start.
    #[error("python init failed: {0}")]
    Init(String),
    /// yt-dlp could not extract or download (often fixed by updating yt-dlp).
    #[error("yt-dlp: {0}")]
    Extractor(String),
    /// Any other exception raised on the Python side.
    #[error("python: {0}")]
    Python(String),
    #[error("cancelled")]
    Cancelled,
    #[error("runtime already started in this process")]
    AlreadyStarted,
    #[error("runtime worker stopped")]
    Stopped,
    #[error("unexpected yt-dlp output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("tagging {path}: {message}")]
    Tag { path: PathBuf, message: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// Errors that a newer yt-dlp may fix.
    pub fn is_extractor(&self) -> bool {
        matches!(self, Error::Extractor(_))
    }

    /// The video is gone, private or blocked for this viewer: trying again won't help.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Error::Extractor(msg) if is_unavailable_message(msg))
    }
}

/// [`Error::is_unavailable`] for an error that was turned into text.
pub fn is_unavailable_message(msg: &str) -> bool {
    const SIGNS: [&str; 7] = [
        "video unavailable",
        "video is not available",
        "private video",
        "has been removed",
        "account associated with this video has been terminated",
        "not made this video available in your country",
        "sign in to confirm your age",
    ];
    let msg = msg.to_lowercase();
    SIGNS.iter().any(|s| msg.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_unavailable_videos_apart() {
        let gone = Error::Extractor("[youtube] ZPiH8rU0o4s: Video unavailable".into());
        assert!(gone.is_unavailable());
        assert!(is_unavailable_message("[youtube] x: Private video. Sign in if you've been granted access"));
        assert!(!Error::Extractor("unable to download video data: HTTP Error 403: Forbidden".into()).is_unavailable());
        assert!(!Error::Python("Video unavailable".into()).is_unavailable());
    }
}
