use thiserror::Error;

#[derive(Error, Debug)]
pub enum DzipError {
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),

    #[error("IO Error in context '{0}': {1}")]
    IoContext(String, #[source] std::io::Error),

    #[error("Invalid Magic Header: expected DZIP, found {0:#x}")]
    InvalidMagic(u32),

    #[error("Compression Error: {0}")]
    Compression(String),

    #[error("Decompression Error: {0}")]
    Decompression(String),

    #[error("Configuration Error: {0}")]
    Config(String),

    #[error("TOML Serialization Error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML Deserialization Error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Chunk Definition Missing for ID {0}")]
    ChunkDefinitionMissing(u16),

    #[error("Split file missing or invalid: {0:?}")]
    SplitFileMissing(std::path::PathBuf),

    #[error("Generic Error: {0}")]
    Generic(String),

    #[error("Thread Panic: {0}")]
    ThreadPanic(String),

    #[error("Internal Logic Error: {0}")]
    InternalLogic(String),
}
