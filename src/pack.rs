use anyhow::{Context, Result, anyhow};
use byteorder::{LittleEndian, WriteBytesExt};
use log::info;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;

use crate::compression::CodecRegistry;
use crate::constants::{CHUNK_LIST_TERMINATOR, ChunkFlags, DEFAULT_BUFFER_SIZE, MAGIC};
use crate::error::DzipError;
use crate::types::{ChunkDef, Config};
use crate::utils::encode_flags;

pub fn do_pack(config_path: &PathBuf, registry: &CodecRegistry) -> Result<()> {
    let toml_content = fs::read_to_string(config_path)
        .context(format!("Failed to read config file: {:?}", config_path))?;
    let config: Config =
        toml::from_str(&toml_content).context("Failed to parse TOML configuration")?;

    let base_dir = config_path
        .file_stem()
        .ok_or_else(|| anyhow!("Invalid config filename"))?
        .to_string_lossy()
        .to_string();

    let base_path = config_path
        .parent()
        .ok_or_else(|| anyhow!("Cannot determine parent directory of config file"))?
        .join(&base_dir);

    info!("Packing from directory: {:?}", base_path);

    let mut chunk_map_def: HashMap<u16, &ChunkDef> = HashMap::new();
    let mut has_dz_chunk = false;
    for c in &config.chunks {
        chunk_map_def.insert(c.id, c);
        let flags = ChunkFlags::from_bits_truncate(encode_flags(&c.flags));
        if flags.contains(ChunkFlags::DZ_RANGE) {
            has_dz_chunk = true;
        }
    }

    // 1. Index Source Files
    info!("Indexing source files...");
    let mut chunk_source_map: HashMap<u16, (Arc<PathBuf>, u64, usize)> = HashMap::new();

    for f_entry in &config.files {
        let mut clean_rel_path = PathBuf::new();
        for part in f_entry.path.split(['/', '\\']) {
            if part == "." || part.is_empty() {
                continue;
            }
            if part == ".." {
                clean_rel_path.pop();
            } else {
                clean_rel_path.push(part);
            }
        }
        let full_path = base_path.join(clean_rel_path);

        if !full_path.exists() {
            return Err(
                DzipError::Config(format!("Source file not found: {:?}", full_path)).into(),
            );
        }

        let full_path_arc = Arc::new(full_path);
        let mut current_offset: u64 = 0;
        for cid in &f_entry.chunks {
            let c_def = chunk_map_def.get(cid).ok_or_else(|| {
                DzipError::Config(format!("Chunk ID {} undefined in [chunks]", cid))
            })?;

            let flags = ChunkFlags::from_bits_truncate(encode_flags(&c_def.flags));
            let read_len = if flags.contains(ChunkFlags::DZ_RANGE) {
                c_def.size_compressed
            } else {
                c_def.size_decompressed
            } as usize;

            chunk_source_map.insert(*cid, (full_path_arc.clone(), current_offset, read_len));
            current_offset += read_len as u64;
        }
    }

    // 2. Build Preliminary Header
    let mut unique_dirs = HashSet::new();
    for f in &config.files {
        let d = f.directory.trim();
        if d.is_empty() || d == "." {
            unique_dirs.insert(".".to_string());
        } else {
            unique_dirs.insert(d.replace('\\', "/"));
        }
    }
    if !unique_dirs.contains(".") {
        unique_dirs.insert(".".to_string());
    }
    let mut sorted_dirs: Vec<String> = unique_dirs.into_iter().collect();
    sorted_dirs.sort();
    if let Some(pos) = sorted_dirs.iter().position(|x| x == ".") {
        sorted_dirs.remove(pos);
    }
    sorted_dirs.insert(0, ".".to_string());
    let dir_map: HashMap<String, usize> = sorted_dirs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.clone(), i))
        .collect();

    let mut header_buffer = Cursor::new(Vec::new());
    header_buffer.write_u32::<LittleEndian>(MAGIC)?;
    header_buffer.write_u16::<LittleEndian>(config.files.len() as u16)?;
    header_buffer.write_u16::<LittleEndian>(sorted_dirs.len() as u16)?;
    header_buffer.write_u8(0)?;

    for f in &config.files {
        header_buffer.write_all(f.filename.as_bytes())?;
        header_buffer.write_u8(0)?;
    }
    for d in sorted_dirs.iter().skip(1) {
        header_buffer.write_all(d.replace('/', "\\").as_bytes())?;
        header_buffer.write_u8(0)?;
    }
    for f in &config.files {
        let raw_d = f.directory.replace('\\', "/");
        let d_key = if raw_d.is_empty() || raw_d == "." {
            "."
        } else {
            &raw_d
        };
        let d_id = *dir_map.get(d_key).unwrap_or(&0) as u16;
        header_buffer.write_u16::<LittleEndian>(d_id)?;
        for cid in &f.chunks {
            header_buffer.write_u16::<LittleEndian>(*cid)?;
        }
        header_buffer.write_u16::<LittleEndian>(CHUNK_LIST_TERMINATOR)?;
    }
    header_buffer.write_u16::<LittleEndian>((1 + config.archive_files.len()) as u16)?;
    header_buffer.write_u16::<LittleEndian>(config.chunks.len() as u16)?;

    let chunk_table_start = header_buffer.position();
    for _ in 0..config.chunks.len() {
        for _ in 0..16 {
            header_buffer.write_u8(0)?;
        }
    }
    if !config.archive_files.is_empty() {
        for fname in &config.archive_files {
            header_buffer.write_all(fname.as_bytes())?;
            header_buffer.write_u8(0)?;
        }
    }

    if has_dz_chunk {
        if let Some(rs) = &config.range_settings {
            header_buffer.write_u8(rs.win_size)?;
            header_buffer.write_u8(rs.flags)?;
            header_buffer.write_u8(rs.offset_table_size)?;
            header_buffer.write_u8(rs.offset_tables)?;
            header_buffer.write_u8(rs.offset_contexts)?;
            header_buffer.write_u8(rs.ref_length_table_size)?;
            header_buffer.write_u8(rs.ref_length_tables)?;
            header_buffer.write_u8(rs.ref_offset_table_size)?;
            header_buffer.write_u8(rs.ref_offset_tables)?;
            header_buffer.write_u8(rs.big_min_match)?;
        } else {
            for _ in 0..10 {
                header_buffer.write_u8(0)?;
            }
        }
    }

    let out_filename_0 = format!("{}_packed.dz", base_dir);
    let mut current_offset_0 = header_buffer.position() as u32;
    let f0 = File::create(&out_filename_0)?;

    let mut writer0 = BufWriter::with_capacity(DEFAULT_BUFFER_SIZE, f0);
    writer0.write_all(header_buffer.get_ref())?;

    let mut split_writers: HashMap<u16, BufWriter<File>> = HashMap::new();
    let mut split_offsets: HashMap<u16, u32> = HashMap::new();

    let config_parent = config_path
        .parent()
        .ok_or_else(|| anyhow!("Config path has no parent"))?;

    for (i, fname) in config.archive_files.iter().enumerate() {
        let idx = (i + 1) as u16;
        let path = config_parent.join(fname);
        let f = File::create(&path)?;

        split_writers.insert(idx, BufWriter::with_capacity(DEFAULT_BUFFER_SIZE, f));
        split_offsets.insert(idx, 0);
    }

    // 4. Stream Data (Pipeline: Producer -> Channel -> Writer Thread)
    let mut sorted_chunks_def = config.chunks.clone();
    sorted_chunks_def.sort_by_key(|c| c.id);

    info!(
        "Compressing {} chunks (Pipeline)...",
        sorted_chunks_def.len()
    );

    struct CompressionJob {
        chunk_idx: usize,
        source_path: Arc<PathBuf>,
        offset: u64,
        read_len: usize,
        flags: Vec<std::borrow::Cow<'static, str>>,
    }

    // [Optimization]: Handled potential error map lookup safely using Result
    let jobs: Result<Vec<CompressionJob>> = sorted_chunks_def
        .iter()
        .enumerate()
        .map(|(i, c_def)| {
            let (source_path, src_offset, read_len) = chunk_source_map
                .get(&c_def.id)
                .ok_or_else(|| anyhow!("Source map missing for chunk ID {}", c_def.id))?;

            Ok(CompressionJob {
                chunk_idx: i,
                source_path: source_path.clone(),
                offset: *src_offset,
                read_len: *read_len,
                flags: c_def.flags.clone(),
            })
        })
        .collect();

    let jobs = jobs?; // Propagate error if chunk map was inconsistent

    let channel_bound = rayon::current_num_threads() * 4;
    let (tx, rx) = mpsc::sync_channel::<(usize, Result<Vec<u8>>)>(channel_bound);

    // Spawn Writer Thread
    let writer_handle = thread::spawn(move || -> Result<(Vec<ChunkDef>, BufWriter<File>)> {
        let total_chunks = sorted_chunks_def.len();
        let mut buffer: HashMap<usize, Vec<u8>> = HashMap::new();
        let mut next_idx = 0;

        while next_idx < total_chunks {
            // Check if next chunk is already buffered
            let data = if let Some(d) = buffer.remove(&next_idx) {
                d
            } else {
                // Not buffered, wait for it
                match rx.recv() {
                    Ok((idx, res)) => {
                        let chunk_data = res?;
                        if idx == next_idx {
                            chunk_data
                        } else {
                            // Out of order arrival, buffer it
                            buffer.insert(idx, chunk_data);
                            continue;
                        }
                    }
                    Err(_) => {
                        return Err(anyhow!(
                            "Compression threads disconnected before finishing all chunks"
                        ));
                    }
                }
            };

            // Write Logic
            let c_def = &mut sorted_chunks_def[next_idx];
            let target_writer = if c_def.archive_file_index == 0 {
                &mut writer0
            } else {
                split_writers
                    .get_mut(&c_def.archive_file_index)
                    .ok_or_else(|| {
                        DzipError::Config(format!(
                            "Chunk {} refers to non-existent archive_file_index: {}",
                            c_def.id, c_def.archive_file_index
                        ))
                    })?
            };

            let current_pos = if c_def.archive_file_index == 0 {
                current_offset_0
            } else {
                *split_offsets
                    .get(&c_def.archive_file_index)
                    .ok_or_else(|| {
                        DzipError::Config(format!(
                            "Missing offset tracking for archive_file_index: {}",
                            c_def.archive_file_index
                        ))
                    })?
            };

            target_writer.write_all(&data)?;

            c_def.offset = current_pos;
            c_def.size_compressed = data.len() as u32;

            if c_def.archive_file_index == 0 {
                current_offset_0 += c_def.size_compressed;
            } else {
                let offset_ref = split_offsets
                    .get_mut(&c_def.archive_file_index)
                    .ok_or_else(|| {
                        DzipError::Config(format!(
                            "Missing offset tracking for archive_file_index: {}",
                            c_def.archive_file_index
                        ))
                    })?;
                *offset_ref += c_def.size_compressed;
            }

            next_idx += 1;
        }

        for w in split_writers.values_mut() {
            w.flush()?;
        }

        Ok((sorted_chunks_def, writer0))
    });

    // Run Compression Jobs (Producers)
    jobs.par_iter().for_each_with(tx, |s, job| {
        let res = (|| -> Result<Vec<u8>> {
            let mut f_in = File::open(job.source_path.as_ref())?;
            f_in.seek(SeekFrom::Start(job.offset))?;

            let buffered_reader = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, f_in);
            let mut chunk_reader = buffered_reader.take(job.read_len as u64);

            let mut compressed_buffer = Vec::new();
            let flags_int = encode_flags(&job.flags);

            registry.compress(&mut chunk_reader, &mut compressed_buffer, flags_int)?;
            Ok(compressed_buffer)
        })();

        let _ = s.send((job.chunk_idx, res));
    });

    let (final_chunks_def, mut writer0) = writer_handle
        .join()
        .map_err(|e| anyhow!("Writer thread panicked: {:?}", e))??;

    let mut table_writer = Cursor::new(Vec::new());
    for c in &final_chunks_def {
        table_writer.write_u32::<LittleEndian>(c.offset)?;
        table_writer.write_u32::<LittleEndian>(c.size_compressed)?;
        table_writer.write_u32::<LittleEndian>(c.size_decompressed)?;
        table_writer.write_u16::<LittleEndian>(encode_flags(&c.flags))?;
        table_writer.write_u16::<LittleEndian>(c.archive_file_index)?;
    }

    writer0.seek(SeekFrom::Start(chunk_table_start))?;
    writer0.write_all(table_writer.get_ref())?;
    writer0.flush()?;

    info!("All files packed successfully.");
    Ok(())
}
