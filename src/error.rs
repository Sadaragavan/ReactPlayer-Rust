//! Error types shared by every layer of the application.
//!
//! Media and platform errors are converted into [`AppError`] here so that the UI
//! can report them in a toast instead of panicking. Nothing in the playback
//! path is allowed to abort the process on a recoverable failure.

use std::fmt;

/// A user-presentable error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppError {
    /// Short, human-readable message.
    pub message: String,
}

impl AppError {
    /// Create a new error from anything displayable.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl From<ffmpeg_next::Error> for AppError {
    fn from(value: ffmpeg_next::Error) -> Self {
        Self::new(format!("FFmpeg: {value}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::new(format!("I/O: {value}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(format!("JSON: {value}"))
    }
}

impl From<arboard::Error> for AppError {
    fn from(value: arboard::Error) -> Self {
        Self::new(format!("Clipboard: {value}"))
    }
}

impl From<cpal::Error> for AppError {
    fn from(value: cpal::Error) -> Self {
        Self::new(format!("Audio: {value}"))
    }
}

impl From<image::ImageError> for AppError {
    fn from(value: image::ImageError) -> Self {
        Self::new(format!("Image: {value}"))
    }
}

impl From<ffmpeg_next::Error> for Box<AppError> {
    fn from(value: ffmpeg_next::Error) -> Self {
        Box::new(AppError::from(value))
    }
}

/// Convenience alias.
pub type AppResult<T> = Result<T, AppError>;
