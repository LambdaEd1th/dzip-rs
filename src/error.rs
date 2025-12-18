use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DzipError {
    #[error("I/O Error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Format Error: Invalid Magic Header. Expected 'DTRZ' (0x5A525444), found 0x{0:X}")]
    InvalidMagic(u32),

    #[error("Security Error: {0}")]
    Security(String),

    #[error("Configuration Error: {0}")]
    Config(String),

    #[error("Missing Resource: Split archive part not found: {0:?}")]
    SplitFileMissing(PathBuf),

    #[error("Unsupported Feature: {0}")]
    Unsupported(String),

    #[error("Decompression Failed: {0}")]
    Decompression(String),
}
