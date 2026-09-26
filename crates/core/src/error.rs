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
}
