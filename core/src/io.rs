use crate::Result;
use std::io::{Read, Seek, Write};

// --- Stream Trait Aliases ---

/// A stream that can Read, Seek, and be sent across threads (required for Rayon).
pub trait ReadSeekSend: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeekSend for T {}

/// A stream that can Write, Seek, and be sent across threads.
pub trait WriteSeekSend: Write + Seek + Send {}
impl<T: Write + Seek + Send> WriteSeekSend for T {}

/// A stream that can Write and be sent across threads (Seek not required).
pub trait WriteSend: Write + Send {}
impl<T: Write + Send> WriteSend for T {}

// --- Unpack Interfaces ---

/// Abstraction for the source of archive data (The .dz file and its splits).
pub trait UnpackSource: Send + Sync {
    /// Open the main archive file for reading.
    fn open_main(&self) -> Result<Box<dyn ReadSeekSend>>;

    /// Open a split file (e.g., .d01) for reading.
    fn open_split(&self, split_name: &str) -> Result<Box<dyn ReadSeekSend>>;

    /// Get the size of a split file (needed for chunk size correction).
    fn get_split_len(&self, split_name: &str) -> Result<u64>;
}

/// Abstraction for the destination of extracted files.
pub trait UnpackSink: Send + Sync {
    /// Create a directory (and parents) given a logical relative path.
    fn create_dir_all(&self, rel_path: &str) -> Result<()>;

    /// Create a file for writing given a logical relative path.
    fn create_file(&self, rel_path: &str) -> Result<Box<dyn WriteSend>>;
}

// --- Pack Interfaces ---

/// Abstraction for the source of raw files to be packed.
pub trait PackSource: Send + Sync {
    /// Check if a file exists given its logical relative path.
    fn exists(&self, rel_path: &str) -> bool;

    /// Open a source file for reading.
    fn open_file(&self, rel_path: &str) -> Result<Box<dyn ReadSeekSend>>;
}

/// Abstraction for the destination of the created archive.
pub trait PackSink: Send + Sync {
    /// Create the main .dz file for writing.
    fn create_main(&mut self) -> Result<Box<dyn WriteSeekSend>>;

    /// Create a split file (e.g., .d01) for writing.
    fn create_split(&mut self, split_idx: u16) -> Result<Box<dyn WriteSeekSend>>;
}
