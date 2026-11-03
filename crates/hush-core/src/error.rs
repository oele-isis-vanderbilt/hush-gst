//! Error type for hush-core. No printing to stderr: callers decide what to do.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("model bundle hash mismatch: expected {expected}, actual {actual}")]
    HashMismatch { expected: String, actual: String },

    /// Bad archive, missing or empty file, unexpected entry, size cap exceeded.
    #[error("invalid model bundle: {0}")]
    Bundle(String),

    /// tract failed to build the model plans.
    #[error("model initialization failed: {0}")]
    ModelInit(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid setting: {0}")]
    InvalidSetting(String),

    #[error("invalid frame size: expected {expected}, got {got}")]
    FrameSize {
        expected: usize,
        got: usize,
    },

    #[error("processing error: {0}")]
    Process(String)
}