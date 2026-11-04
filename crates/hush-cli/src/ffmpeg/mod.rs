//! Finding, running and installing ffmpeg / ffprobe.

pub mod install;
pub mod locate;

pub use locate::{Tools, locate};

/// Platform file name of a tool (`ffmpeg` or `ffmpeg.exe`).
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}
