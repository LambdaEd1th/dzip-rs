use binrw::{BinRead, BinWrite};
use bitflags::bitflags;
use std::io::{Read, Seek, Write};

pub const MAGIC: u32 = 0x5A525444; // 'DTRZ' in Little Endian
pub const CHUNK_LIST_TERMINATOR: u16 = 0xFFFF;
pub const CURRENT_DIR_STR: &str = ".";
pub const DEFAULT_BUFFER_SIZE: usize = 128 * 1024;

// --- Binary Structures ---

/// Main file header
#[derive(Debug, BinRead, BinWrite)]
#[brw(little)] // Applies Little Endian to all fields
#[br(assert(magic == MAGIC, "Invalid Magic: expected {:#x}, found {:#x}", MAGIC, magic))]
pub struct ArchiveHeader {
    pub magic: u32,
    pub num_files: u16,
    pub num_dirs: u16,
    pub version: u8,
}

/// Helper struct for reading file map entries.
#[derive(Debug, BinRead, BinWrite)]
#[brw(little)]
pub struct FileMapDiskEntry {
    pub dir_idx: u16,
    // Custom parser for the variable-length list terminated by 0xFFFF
    #[br(parse_with = read_chunk_ids)]
    #[bw(write_with = write_chunk_ids)]
    pub chunk_ids: Vec<u16>,
}

/// Custom parser for the null-terminated chunk ID list (0xFFFF).
fn read_chunk_ids<R: Read + Seek>(
    reader: &mut R,
    endian: binrw::Endian,
    _: (),
) -> binrw::BinResult<Vec<u16>> {
    let mut ids = Vec::new();
    loop {
        // Use read_options to pass the endianness explicitly
        let id = u16::read_options(reader, endian, ())?;
        if id == CHUNK_LIST_TERMINATOR {
            break;
        }
        ids.push(id);
    }
    Ok(ids)
}

/// Custom writer for the null-terminated chunk ID list.
fn write_chunk_ids<W: Write + Seek>(
    ids: &Vec<u16>,
    writer: &mut W,
    endian: binrw::Endian,
    _: (),
) -> binrw::BinResult<()> {
    for id in ids {
        id.write_options(writer, endian, ())?;
    }
    CHUNK_LIST_TERMINATOR.write_options(writer, endian, ())?;
    Ok(())
}

/// Header for the chunk table section.
#[derive(Debug, BinRead, BinWrite)]
#[brw(little)]
pub struct ChunkTableHeader {
    pub num_arch_files: u16,
    pub num_chunks: u16,
}

/// Represents a single chunk definition in the binary file.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(little)]
pub struct ChunkDiskEntry {
    pub offset: u32,
    pub c_len: u32,
    pub d_len: u32,
    pub flags: u16,
    pub file_idx: u16,
}

/// Advanced compression settings (only present if DZ_RANGE flag is used).
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(little)]
pub struct RangeSettingsDisk {
    pub win_size: u8,
    pub flags: u8,
    pub offset_table_size: u8,
    pub offset_tables: u8,
    pub offset_contexts: u8,
    pub ref_length_table_size: u8,
    pub ref_length_tables: u8,
    pub ref_offset_table_size: u8,
    pub ref_offset_tables: u8,
    pub big_min_match: u8,
}

// --- Flags and Constants ---

pub const FLAG_MAPPINGS: &[(ChunkFlags, &str)] = &[
    (ChunkFlags::COMBUF, "COMBUF"),
    (ChunkFlags::DZ_RANGE, "DZ_RANGE"),
    (ChunkFlags::ZLIB, "ZLIB"),
    (ChunkFlags::BZIP, "BZIP"),
    (ChunkFlags::MP3, "MP3"),
    (ChunkFlags::JPEG, "JPEG"),
    (ChunkFlags::ZERO, "ZERO"),
    (ChunkFlags::COPYCOMP, "COPY"),
    (ChunkFlags::LZMA, "LZMA"),
    (ChunkFlags::RANDOMACCESS, "RANDOM_ACCESS"),
];

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ChunkFlags: u16 {
        const COMBUF       = 0x1;
        const DZ_RANGE     = 0x4;
        const ZLIB         = 0x8;
        const BZIP         = 0x10;
        const MP3          = 0x20;
        const JPEG         = 0x40;
        const ZERO         = 0x80;
        const COPYCOMP     = 0x100;
        const LZMA         = 0x200;
        const RANDOMACCESS = 0x400;
    }
}
