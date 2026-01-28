use crate::DzipError;
use crate::error::Result;
use crate::format::*;
use byteorder::{LittleEndian, WriteBytesExt};
use log::warn;
use serde::{Deserialize, Serialize};
use std::io::{Seek, Write};
use std::str::FromStr;

pub struct DzipWriter<W: Write + Seek> {
    writer: W,
}

impl<W: Write + Seek> DzipWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub fn write_archive_settings(&mut self, settings: &ArchiveSettings) -> Result<()> {
        log::debug!("Writing archive settings: {:?}", settings);
        self.writer.write_u32::<LittleEndian>(settings.header)?; // Should be 0x5A525444
        self.writer
            .write_u16::<LittleEndian>(settings.num_user_files)?;
        self.writer
            .write_u16::<LittleEndian>(settings.num_directories)?;
        self.writer.write_u8(settings.version)?;
        Ok(())
    }

    pub fn write_strings(&mut self, strings: &[String]) -> Result<()> {
        for s in strings {
            self.writer.write_all(s.as_bytes())?;
            self.writer.write_u8(0)?; // null terminator
        }
        Ok(())
    }

    pub fn write_file_chunk_map(&mut self, map: &[(u16, Vec<u16>)]) -> Result<()> {
        for (dir_id, chunks) in map {
            self.writer.write_u16::<LittleEndian>(*dir_id)?;
            for &chunk_id in chunks {
                self.writer.write_u16::<LittleEndian>(chunk_id)?;
            }
            self.writer.write_u16::<LittleEndian>(0xFFFF)?; // Terminator
        }
        Ok(())
    }

    pub fn write_chunk_settings(&mut self, settings: &ChunkSettings) -> Result<()> {
        self.writer
            .write_u16::<LittleEndian>(settings.num_archive_files)?;
        self.writer.write_u16::<LittleEndian>(settings.num_chunks)?;
        Ok(())
    }

    pub fn write_chunks(&mut self, chunks: &[Chunk]) -> Result<()> {
        log::debug!("Writing {} chunks", chunks.len());
        for chunk in chunks {
            self.writer.write_u32::<LittleEndian>(chunk.offset)?;
            self.writer
                .write_u32::<LittleEndian>(chunk.compressed_length)?;
            self.writer
                .write_u32::<LittleEndian>(chunk.decompressed_length)?;
            self.writer.write_u16::<LittleEndian>(chunk.flags)?;
            self.writer.write_u16::<LittleEndian>(chunk.file)?;
        }
        Ok(())
    }

    pub fn write_global_settings(&mut self, settings: &RangeSettings) -> Result<()> {
        self.writer.write_u8(settings.win_size)?;
        self.writer.write_u8(settings.flags)?;
        self.writer.write_u8(settings.offset_table_size)?;
        self.writer.write_u8(settings.offset_tables)?;
        self.writer.write_u8(settings.offset_contexts)?;
        self.writer.write_u8(settings.ref_length_table_size)?;
        self.writer.write_u8(settings.ref_length_tables)?;
        self.writer.write_u8(settings.ref_offset_table_size)?;
        self.writer.write_u8(settings.ref_offset_tables)?;
        self.writer.write_u8(settings.big_min_match)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionMethod {
    Dz,
    Bzip,
    Zlib,
    Copy,
    Zero,
    Mp3,
    Jpeg,
    Lzma,
    Combuf,
    RandomAccess,
}

impl FromStr for CompressionMethod {
    type Err = crate::DzipError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "dz" => Ok(CompressionMethod::Dz),
            "bzip" => Ok(CompressionMethod::Bzip),
            "zlib" => Ok(CompressionMethod::Zlib),
            "copy" => Ok(CompressionMethod::Copy),
            "zero" => Ok(CompressionMethod::Zero),
            "mp3" => Ok(CompressionMethod::Mp3),
            "jpeg" | "jpg" => Ok(CompressionMethod::Jpeg),
            "lzma" => Ok(CompressionMethod::Lzma),
            "combuf" => Ok(CompressionMethod::Combuf),
            "randomaccess" => Ok(CompressionMethod::RandomAccess),
            _ => Err(DzipError::Io(std::io::Error::other(format!(
                "Unknown compression method: {}",
                s
            )))),
        }
    }
}

pub fn compress_data(data: &[u8], method: CompressionMethod) -> Result<(u16, Vec<u8>)> {
    match method {
        CompressionMethod::Copy => Ok((CHUNK_COPYCOMP, data.to_vec())),
        CompressionMethod::Zero => Ok((CHUNK_ZERO, Vec::new())), // Zero chunk has 0 compressed size
        CompressionMethod::Zlib => {
            use flate2::Compression;
            use flate2::write::GzEncoder;
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(data).map_err(DzipError::Io)?;
            Ok((CHUNK_ZLIB, encoder.finish().map_err(DzipError::Io)?))
        }
        CompressionMethod::Bzip => {
            use bzip2::Compression;
            use bzip2::write::BzEncoder;
            let mut encoder = BzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(data).map_err(DzipError::Io)?;
            Ok((CHUNK_BZIP, encoder.finish().map_err(DzipError::Io)?))
        }
        CompressionMethod::Lzma => {
            // lzma-rs
            let mut output = Vec::new();
            lzma_rs::lzma_compress(&mut std::io::Cursor::new(data), &mut output)
                .map_err(|e| DzipError::Io(std::io::Error::other(e)))?;
            Ok((CHUNK_LZMA, output))
        }
        // Fallback to Copy for unsupported types
        _ => {
            warn!("Unsupported compression {:?}, using Copy", method);
            Ok((CHUNK_COPYCOMP, data.to_vec()))
        }
    }
}
