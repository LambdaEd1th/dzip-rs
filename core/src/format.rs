//! Implementation of structures defined in DZSettings.h
//!
//! Version 0 file format is:
//! - ArchiveSettings
//! - User File List (ArchiveSettings.NumUserFiles list of null-terminated files)
//! - DirectoryList (ArchiveSettings.NumDirectories list of null-terminated files)
//! - User-File to Chunk-And-Directory list
//!
//! - ChunkSettings
//! - Chunk List (ChunkSettings.NumChunks list of Chunk structures)
//! - File List (ChunkSettings.NumArchiveFiles -1 list of null-terminated files)
//!
//! - Various global decoder settings...
//!
//! - File data

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveSettings {
    /// Identification 'DTRZ'
    pub header: u32,
    /// Number of original user-files stored in this archive
    pub num_user_files: u16,
    /// Number of stored directories.
    /// Note: The first directory is always the root directory.
    pub num_directories: u16,
    /// Version ID of this settings structure
    pub version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSettings {
    /// Number of files used to store this archive
    pub num_archive_files: u16,
    /// Number of chunks they're divided up into
    pub num_chunks: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// The location of the chunk in its file
    pub offset: u32,
    /// Length of compressed chunk (mainly for use of combufs)
    pub compressed_length: u32,
    /// Length of original data.
    /// Note: In some dzip files, this may be equal to `compressed_length` (both storing the uncompressed size).
    pub decompressed_length: u32,
    /// Chunk flags
    pub flags: u16,
    /// Which file this chunk's compressed data lives in
    pub file: u16,
}

// Chunk flags constants
pub const CHUNK_COMBUF: u16 = 0x1; // Set to indicate a combuf chunk.
pub const CHUNK_DZ: u16 = 0x4; // Set to indicate a dzip chunk, for use with range decoder
pub const CHUNK_ZLIB: u16 = 0x8; // Set to indicate a zlib (or gzip) chunk
pub const CHUNK_BZIP: u16 = 0x10; // Set to indicate a bzip2 chunk
pub const CHUNK_MP3: u16 = 0x20; // Set to indicate a mp3 chunk
pub const CHUNK_JPEG: u16 = 0x40; // Set to indicate a JPEG chunk
pub const CHUNK_ZERO: u16 = 0x80; // Set to indicate a zerod-out chunk
pub const CHUNK_COPYCOMP: u16 = 0x100; // Set to indicate a copy-coded (ie no compression) chunk
pub const CHUNK_LZMA: u16 = 0x200; // Set to indicate a lzma encoded chunk
pub const CHUNK_RANDOMACCESS: u16 = 0x400; // Set to indicate whole chunk should be buffered for random access

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSettings {
    /// log2(LZ-77 window size)
    pub win_size: u8,
    /// Settings for rangedecoding
    pub flags: u8,
    /// log2(LZ-77 match offset frequency table size)
    pub offset_table_size: u8,
    /// number of LZ-77 offset frequency tables
    pub offset_tables: u8,
    /// number of different (length-based) contexts for predicting LZ-77 offsets
    pub offset_contexts: u8,
    /// log2(external reference length frequency table size)
    pub ref_length_table_size: u8,
    /// number of external reference length frequency tables
    pub ref_length_tables: u8,
    /// log2(external reference offset frequency table size)
    pub ref_offset_table_size: u8,
    /// number of external reference offset frequency tables
    pub ref_offset_tables: u8,
    /// minimum match length for external references
    pub big_min_match: u8,
}
