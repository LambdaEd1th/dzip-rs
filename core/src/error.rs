use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DzipError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Invalid DTRZ header")]
    InvalidHeader,

    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("Unsupported compression method: flags={0:#x}")]
    UnsupportedCompression(u16),
}

pub type Result<T> = std::result::Result<T, DzipError>;
