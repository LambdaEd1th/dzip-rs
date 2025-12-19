use anyhow::{Context, Result, anyhow};
use byteorder::{LittleEndian, ReadBytesExt};
use log::{info, warn};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{MAIN_SEPARATOR_STR, PathBuf};

use crate::compression::CodecRegistry;
use crate::constants::{CHUNK_LIST_TERMINATOR, ChunkFlags, DEFAULT_BUFFER_SIZE, MAGIC};
use crate::error::DzipError;
use crate::types::{ArchiveMeta, ChunkDef, Config, FileEntry, RangeSettings};
use crate::utils::{decode_flags, read_null_term_string, sanitize_path};

pub fn do_unpack(
    input_path: &PathBuf,
    out_opt: Option<PathBuf>,
    keep_raw: bool,
    registry: &CodecRegistry,
) -> Result<()> {
    // Open the main archive file
    let main_file_raw = File::open(input_path)
        .map_err(DzipError::Io)
        .context(format!("Failed to open main archive: {:?}", input_path))?;

    let main_file_len = main_file_raw.metadata()?.len();
    let mut main_file = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, main_file_raw);

    // 1. Read Header
    let magic = main_file.read_u32::<LittleEndian>()?;
    if magic != MAGIC {
        return Err(DzipError::InvalidMagic(magic).into());
    }
    let num_files = main_file.read_u16::<LittleEndian>()?;
    let num_dirs = main_file.read_u16::<LittleEndian>()?;
    let version = main_file.read_u8()?;

    info!(
        "Header: Ver {}, Files {}, Dirs {}",
        version, num_files, num_dirs
    );

    // 2. Read String Table (Filenames)
    let mut user_files = Vec::new();
    for _ in 0..num_files {
        user_files.push(read_null_term_string(&mut main_file)?);
    }

    // Read String Table (Directories)
    let mut directories = Vec::new();
    directories.push(".".to_string());
    for _ in 0..(num_dirs - 1) {
        directories.push(read_null_term_string(&mut main_file)?);
    }

    // 3. Read Mapping Table
    struct FileMapEntry {
        id: usize,
        dir_idx: usize,
        chunk_ids: Vec<u16>,
    }
    let mut map_entries = Vec::new();
    for i in 0..num_files {
        let dir_id = main_file.read_u16::<LittleEndian>()? as usize;
        let mut chunks = Vec::new();
        loop {
            let cid = main_file.read_u16::<LittleEndian>()?;
            if cid == CHUNK_LIST_TERMINATOR {
                break;
            }
            chunks.push(cid);
        }
        map_entries.push(FileMapEntry {
            id: i as usize,
            dir_idx: dir_id,
            chunk_ids: chunks,
        });
    }

    // 4. Read Chunk Settings
    let num_arch_files = main_file.read_u16::<LittleEndian>()?;
    let num_chunks = main_file.read_u16::<LittleEndian>()?;
    info!(
        "Chunk Settings: {} chunks in {} archive files",
        num_chunks, num_arch_files
    );

    // 5. Read Chunk List
    #[derive(Clone)]
    struct RawChunk {
        id: u16,
        offset: u32,
        _head_c_len: u32,
        d_len: u32,
        flags: u16,
        file_idx: u16,
        real_c_len: u32,
    }
    let mut chunks = Vec::new();
    let mut has_dz_chunk = false;

    for i in 0..num_chunks {
        let offset = main_file.read_u32::<LittleEndian>()?;
        let c_len = main_file.read_u32::<LittleEndian>()?;
        let d_len = main_file.read_u32::<LittleEndian>()?;
        let flags_raw = main_file.read_u16::<LittleEndian>()?;
        let file_idx = main_file.read_u16::<LittleEndian>()?;

        let flags = ChunkFlags::from_bits_truncate(flags_raw);
        if flags.contains(ChunkFlags::DZ_RANGE) {
            has_dz_chunk = true;
        }

        chunks.push(RawChunk {
            id: i,
            offset,
            _head_c_len: c_len,
            d_len,
            flags: flags_raw,
            file_idx,
            real_c_len: 0,
        });
    }

    // 6. Read Split Filenames
    let mut split_file_names = Vec::new();
    if num_arch_files > 1 {
        info!("Reading {} split archive filenames...", num_arch_files - 1);
        for _ in 0..(num_arch_files - 1) {
            split_file_names.push(read_null_term_string(&mut main_file)?);
        }
    }

    // 7. Read RangeSettings (if DZ chunk exists)
    let mut range_settings_opt = None;
    if has_dz_chunk {
        info!("Detected CHUNK_DZ, reading RangeSettings...");
        range_settings_opt = Some(RangeSettings {
            win_size: main_file.read_u8()?,
            flags: main_file.read_u8()?,
            offset_table_size: main_file.read_u8()?,
            offset_tables: main_file.read_u8()?,
            offset_contexts: main_file.read_u8()?,
            ref_length_table_size: main_file.read_u8()?,
            ref_length_tables: main_file.read_u8()?,
            ref_offset_table_size: main_file.read_u8()?,
            ref_offset_tables: main_file.read_u8()?,
            big_min_match: main_file.read_u8()?,
        });
    }

    // --- ZSIZE Correction ---
    let base_dir = input_path.parent().unwrap_or(std::path::Path::new("."));
    let mut file_chunks_map: HashMap<u16, Vec<usize>> = HashMap::new();
    for (idx, c) in chunks.iter().enumerate() {
        file_chunks_map.entry(c.file_idx).or_default().push(idx);
    }

    for (f_idx, c_indices) in file_chunks_map.iter() {
        let mut sorted_indices = c_indices.clone();
        sorted_indices.sort_by_key(|&i| chunks[i].offset);

        let current_file_size = if *f_idx == 0 {
            main_file_len
        } else {
            let idx = (*f_idx - 1) as usize;
            let split_name = split_file_names
                .get(idx)
                .ok_or_else(|| anyhow!("Invalid split file index {} in header", f_idx))?;
            let split_path = base_dir.join(split_name);
            fs::metadata(&split_path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        DzipError::SplitFileMissing(split_path.clone())
                    } else {
                        DzipError::Io(e)
                    }
                })?
                .len()
        };

        for k in 0..sorted_indices.len() {
            let idx = sorted_indices[k];
            let current_offset = chunks[idx].offset;
            let next_offset = if k == sorted_indices.len() - 1 {
                current_file_size as u32
            } else {
                chunks[sorted_indices[k + 1]].offset
            };

            if next_offset < current_offset {
                chunks[idx].real_c_len = chunks[idx]._head_c_len;
            } else {
                chunks[idx].real_c_len = next_offset - current_offset;
            }
        }
    }

    let chunk_indices: HashMap<u16, usize> =
        chunks.iter().enumerate().map(|(i, c)| (c.id, i)).collect();

    let base_name = input_path
        .file_stem()
        .ok_or_else(|| anyhow!("Invalid input file path"))?
        .to_string_lossy();
    let root_out = out_opt.unwrap_or_else(|| PathBuf::from(&base_name.to_string()));
    fs::create_dir_all(&root_out)?;

    // 8. Start Extraction (Parallel & Buffered, with Thread-Local File Cache)
    info!(
        "Extracting {} files to {:?} (Parallel, Buffered)...",
        map_entries.len(),
        root_out
    );

    map_entries.par_iter().try_for_each_init(
        HashMap::new, // [Fix]: Use function pointer instead of redundant closure
        |file_cache, entry| -> Result<()> {
            let fname = &user_files[entry.id];
            let raw_dir = if entry.dir_idx < directories.len() {
                &directories[entry.dir_idx]
            } else {
                "."
            };
            let full_raw_path = if raw_dir == "." || raw_dir.is_empty() {
                fname.clone()
            } else {
                format!("{}/{}", raw_dir, fname)
            };

            let disk_path = sanitize_path(&root_out, &full_raw_path)?;
            let rel_path_display = full_raw_path.replace(['/', '\\'], MAIN_SEPARATOR_STR);

            if let Some(parent) = disk_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let out_file = File::create(&disk_path)?;
            let mut writer = BufWriter::with_capacity(DEFAULT_BUFFER_SIZE, out_file);

            for cid in &entry.chunk_ids {
                if let Some(&idx) = chunk_indices.get(cid) {
                    let chunk = &chunks[idx];

                    // --- [Optimized] Thread-Local File Caching with Safety Checks ---
                    // [Fix]: Use entry API to avoid double lookup and Clippy warning
                    let source_file = match file_cache.entry(chunk.file_idx) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            let f = if chunk.file_idx == 0 {
                                File::open(input_path).map_err(DzipError::Io)?
                            } else {
                                // [Safety]: Check array bounds for split files to avoid panic
                                let split_idx = (chunk.file_idx - 1) as usize;
                                let split_name =
                                    split_file_names.get(split_idx).ok_or_else(|| {
                                        anyhow!(
                                            "Invalid archive file index {} for chunk {}",
                                            chunk.file_idx,
                                            chunk.id
                                        )
                                    })?;

                                let split_path = base_dir.join(split_name);
                                File::open(&split_path).map_err(|e| {
                                    if e.kind() == std::io::ErrorKind::NotFound {
                                        DzipError::SplitFileMissing(split_path.clone())
                                    } else {
                                        DzipError::Io(e)
                                    }
                                })?
                            };
                            e.insert(f)
                        }
                    };

                    source_file.seek(SeekFrom::Start(chunk.offset as u64))?;

                    let buffered_reader =
                        BufReader::with_capacity(DEFAULT_BUFFER_SIZE, source_file);
                    let mut source_reader = buffered_reader.take(chunk.real_c_len as u64);

                    if let Err(e) = registry.decompress(
                        &mut source_reader,
                        &mut writer,
                        chunk.flags,
                        chunk.d_len,
                    ) {
                        // Fallback: copy raw
                        let mut raw_buf_reader = source_reader.into_inner();
                        raw_buf_reader.seek(SeekFrom::Start(chunk.offset as u64))?;
                        let mut raw_take = raw_buf_reader.take(chunk.real_c_len as u64);

                        let c_flags = ChunkFlags::from_bits_truncate(chunk.flags);

                        if c_flags.contains(ChunkFlags::DZ_RANGE) && keep_raw {
                            info!(
                                "Keeping raw data for chunk {} (DZ_RANGE) in {}",
                                chunk.id, rel_path_display
                            );
                            std::io::copy(&mut raw_take, &mut writer)?;
                        } else if c_flags.contains(ChunkFlags::DZ_RANGE) {
                            return Err(DzipError::Unsupported(format!(
                                "Chunk format DZ_RANGE in {}. Use --keep-raw.",
                                rel_path_display
                            ))
                            .into());
                        } else {
                            warn!(
                                "Failed to decompress {}: {}. Writing raw data.",
                                rel_path_display, e
                            );
                            std::io::copy(&mut raw_take, &mut writer)?;
                        }
                    }
                }
            }
            writer.flush()?;
            Ok(())
        },
    )?;

    // 9. Generate TOML Info (Sequential)
    let mut toml_files = Vec::new();
    for entry in &map_entries {
        let fname = &user_files[entry.id];
        let raw_dir = if entry.dir_idx < directories.len() {
            &directories[entry.dir_idx]
        } else {
            "."
        };
        let full_raw_path = if raw_dir == "." || raw_dir.is_empty() {
            fname.clone()
        } else {
            format!("{}/{}", raw_dir, fname)
        };
        let rel_path_display = full_raw_path.replace(['/', '\\'], MAIN_SEPARATOR_STR);
        let dir_display = raw_dir.replace(['/', '\\'], MAIN_SEPARATOR_STR);

        toml_files.push(FileEntry {
            path: rel_path_display,
            directory: dir_display,
            filename: fname.clone(),
            chunks: entry.chunk_ids.clone(),
        });
    }

    // 10. Generate TOML Config
    let mut toml_chunks = Vec::new();
    let mut sorted_chunks_for_toml = chunks;
    sorted_chunks_for_toml.sort_by_key(|c| c.id);

    for c in sorted_chunks_for_toml {
        toml_chunks.push(ChunkDef {
            id: c.id,
            offset: c.offset,
            size_compressed: c.real_c_len,
            size_decompressed: c.d_len,
            flags: decode_flags(c.flags),
            archive_file_index: c.file_idx,
        });
    }

    let config = Config {
        archive: ArchiveMeta {
            version,
            total_files: num_files,
            total_directories: num_dirs,
            total_chunks: num_chunks,
        },
        archive_files: split_file_names.clone(),
        range_settings: range_settings_opt,
        files: toml_files,
        chunks: toml_chunks,
    };

    let config_path = format!("{}.toml", base_name);
    fs::write(&config_path, toml::to_string_pretty(&config)?)?;
    info!("Unpack complete. Config saved to {}", config_path);
    Ok(())
}
