use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not start {tool}: {source}. Is it installed?")]
    ToolMissing {
        tool: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{tool} failed: {message}")]
    ToolFailed { tool: String, message: String },

    #[error("file not found: {0}")]
    NotFound(PathBuf),

    #[error("{0}")]
    Invalid(String),

    #[error("speech-to-text is not available in this build (enable the `whisper` feature)")]
    WhisperUnavailable,

    #[error("transcription failed: {0}")]
    Transcribe(String),

    #[error("download failed: {0}")]
    Download(String),

    #[error("cancelled")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
