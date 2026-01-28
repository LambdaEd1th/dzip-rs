pub mod error;
pub mod format;
pub mod reader;
pub mod writer;

pub use error::{DzipError, Result};
pub use format::{ArchiveSettings, Chunk, ChunkSettings, RangeSettings};
pub use writer::{CompressionMethod, compress_data};

// #[cfg(test)]
// mod tests;
// #[cfg(test)]
// mod tests_real;
