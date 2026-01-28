use clap::{Parser, Subcommand};
use dzip_core::Result;
use dzip_core::{CompressionMethod, compress_data};
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use rayon::prelude::*;

mod config;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Enable verbose logging/output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Unpack a dzip file
    Unpack {
        /// The dzip file to unpack
        input: String,
        /// The output directory
        #[arg(short, long, default_value = ".")]
        output: String,
    },
    /// Pack a directory into a dzip file
    Pack {
        /// The configuration file to pack (toml)
        input: String,
        /// The output directory
        #[arg(short, long, default_value = ".")]
        output: String,
    },
    /// Verify and list archive contents
    Verify {
        /// Input archive file
        input: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let log_level = if cli.verbose { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level)).init();

    match &cli.command {
        Commands::Unpack { input, output } => {
            unpack_archive(input, output)?;
        }
        Commands::Pack { input, output } => {
            info!("Packing from config {} to output dir {}", input, output);
            pack_archive(input, output)?;
        }
        Commands::Verify { input } => {
            verify_archive(input)?;
        }
    }

    Ok(())
}

fn unpack_archive(input_path: &str, output_dir: &str) -> Result<()> {
    let file = std::fs::File::open(input_path)?;
    let mut reader = dzip_core::reader::DzipReader::new(file);

    info!("Reading archive metadata...");
    let settings = reader.read_archive_settings()?;

    // Determine string count (handling implicit root directory)
    let strings_count = (settings.num_user_files + settings.num_directories - 1) as usize;
    let strings = reader.read_strings(strings_count)?;

    let map = reader.read_file_chunk_map(settings.num_user_files as usize)?;
    let chunk_settings = reader.read_chunk_settings()?;
    let mut chunks = reader.read_chunks(chunk_settings.num_chunks as usize)?;

    // Read file list (if multi-volume)
    let num_other_volumes = if chunk_settings.num_archive_files > 0 {
        chunk_settings.num_archive_files as usize - 1
    } else {
        0
    };
    let volume_files = reader.read_file_list(num_other_volumes)?;
    debug!(
        "Num archive files: {}, Volume List: {:?}",
        chunk_settings.num_archive_files, volume_files
    );

    info!(
        "Extracting {} files to '{}'...",
        settings.num_user_files, output_dir
    );
    std::fs::create_dir_all(output_dir)?;

    let mut archives_names = vec![
        std::path::Path::new(input_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
    ];
    archives_names.extend(volume_files.clone());

    use dzip_core::format::CHUNK_DZ;
    let has_dz_chunks = chunks.iter().any(|c| (c.flags & CHUNK_DZ) != 0);

    let global_options = if has_dz_chunks {
        let settings = reader.read_global_settings()?;
        Some(config::GlobalOptions {
            win_size: settings.win_size,
            offset_table_size: settings.offset_table_size,
            offset_tables: settings.offset_tables,
            offset_contexts: settings.offset_contexts,
            ref_length_table_size: settings.ref_length_table_size,
            ref_length_tables: settings.ref_length_tables,
            ref_offset_table_size: settings.ref_offset_table_size,
            ref_offset_tables: settings.ref_offset_tables,
            big_min_match: settings.big_min_match,
            ..config::GlobalOptions::default()
        })
    } else {
        None
    };

    let mut pack_config = config::DzipConfig {
        archives: archives_names,
        base_dir: std::path::PathBuf::from("."),
        files: Vec::new(),
        options: global_options,
    };

    // Prepare Volume Manager
    let input_base_dir = std::path::Path::new(input_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    struct VolumeManager {
        base_dir: std::path::PathBuf,
        file_list: Vec<String>,
        open_files: std::collections::HashMap<u16, std::fs::File>,
    }

    impl dzip_core::reader::VolumeSource for VolumeManager {
        fn open_volume(&mut self, id: u16) -> Result<&mut dyn dzip_core::reader::ReadSeek> {
            use std::collections::hash_map::Entry;

            // id is 1-based index into file_list?
            // Actually, if file=0 it is the main file (handled by reader).
            // If file=1 it is file_list[0].
            if id == 0 {
                // This shouldn't be called for id 0 by read_chunk_data_with_volumes
                return Err(dzip_core::DzipError::Io(std::io::Error::other(
                    "Volume ID 0 is reserved for main file",
                )));
            }

            let list_index = (id - 1) as usize;
            if list_index >= self.file_list.len() {
                return Err(dzip_core::DzipError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Volume ID {} not found in file list", id),
                )));
            }

            match self.open_files.entry(id) {
                Entry::Occupied(e) => Ok(e.into_mut()),
                Entry::Vacant(e) => {
                    let file_name = &self.file_list[list_index];
                    let path = self.base_dir.join(file_name);
                    debug!("Opening volume {}: {}", id, path.display());
                    let file = std::fs::File::open(&path)?;
                    Ok(e.insert(file))
                }
            }
        }
    }

    let volume_manager = VolumeManager {
        base_dir: input_base_dir.to_path_buf(),
        file_list: volume_files, // take ownership
        open_files: std::collections::HashMap::new(),
    };

    // --- Chunk Size Correction ---
    // Some archives (like testnew.dz) have incorrect compressed_length headers (listing uncompressed size).
    // Validity check: compressed_length cannot exceed distance to next chunk or EOF.
    let mut file_sizes = std::collections::HashMap::new();
    if let Ok(meta) = std::fs::metadata(input_path) {
        file_sizes.insert(0u16, meta.len());
    }
    // Access file_list via volume_manager (it took ownership)
    for (i, vol_name) in volume_manager.file_list.iter().enumerate() {
        let path = volume_manager.base_dir.join(vol_name);
        if let Ok(meta) = std::fs::metadata(&path) {
            file_sizes.insert((i + 1) as u16, meta.len());
        }
    }

    let mut chunks_by_file: std::collections::HashMap<u16, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, chunk) in chunks.iter().enumerate() {
        chunks_by_file.entry(chunk.file).or_default().push(i);
    }

    for (file_id, mut indices) in chunks_by_file {
        indices.sort_by_key(|&i| chunks[i].offset);

        let file_size = *file_sizes.get(&file_id).unwrap_or(&0);

        for i in 0..indices.len() {
            let idx = indices[i];
            let chunk_offset = chunks[idx].offset as u64;

            // Determine the limit (end of region)
            let limit = if i + 1 < indices.len() {
                chunks[indices[i + 1]].offset as u64
            } else {
                file_size
            };

            let available = limit.saturating_sub(chunk_offset);

            // If header claims more than available, clamp it.
            // BMS Logic: If SIZE == ZSIZE (equal lengths) for compressed chunks, it means
            // the size is unknown/placeholder, so we SHOULD use the available size (next offset - current).
            use dzip_core::format::{CHUNK_BZIP, CHUNK_DZ, CHUNK_LZMA, CHUNK_ZLIB};
            let is_compressed =
                (chunks[idx].flags & (CHUNK_LZMA | CHUNK_ZLIB | CHUNK_BZIP | CHUNK_DZ)) != 0;
            let equal_sizes = chunks[idx].compressed_length == chunks[idx].decompressed_length;

            if is_compressed && equal_sizes {
                // Always update to available size (whether larger or smaller)
                if chunks[idx].compressed_length != available as u32 {
                    debug!(
                        "Correcting Equal-Size Chunk {} from {} to {} (File {}, Offset {})",
                        idx,
                        chunks[idx].compressed_length,
                        available,
                        chunks[idx].file,
                        chunk_offset
                    );
                    chunks[idx].compressed_length = available as u32;
                }
            } else if (chunks[idx].compressed_length as u64) > available {
                debug!(
                    "Correcting Chunk {} size from {} to {} (File {}, Offset {})",
                    idx, chunks[idx].compressed_length, available, chunks[idx].file, chunk_offset
                );
                chunks[idx].compressed_length = available as u32;
            }
        }
    }
    // -----------------------------

    info!("Extracting {} files to '{}'...", map.len(), output_dir);
    let pb = ProgressBar::new(map.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Collect shared data for parallel execution
    let settings_num_user_files = settings.num_user_files;
    let volume_files_shared = volume_manager.file_list.clone(); // Clone vec from manager
    let input_base_dir_shared = input_base_dir.to_path_buf();

    // We need to collect file entries for config *after* parallel execution or use a mutex.
    // Collecting results is better.
    // Result type: (FileEntry, Vec<String>) where Vec<String> are log messages? No, just log directly or return errors.
    // Actually, we need to generate `pack_config.files`.

    let results: Vec<config::FileEntry> = map
        .par_iter()
        .enumerate()
        .map(|(i, (dir_id, chunk_ids))| -> Result<config::FileEntry> {
            pb.inc(1);
            let file_name = &strings[i];

            let mut file_path = std::path::PathBuf::from(output_dir);
            let mut relative_path_buf = std::path::PathBuf::new();

            if *dir_id > 0 {
                // dir_id 0 is root.
                let dir_index = settings_num_user_files as usize + (*dir_id as usize) - 1;
                if dir_index < strings.len() {
                    let dir_name = &strings[dir_index];
                    file_path.push(dir_name);
                    relative_path_buf.push(dir_name);
                } else {
                    warn!("Invalid directory ID {} for file {}", dir_id, file_name);
                }
            }
            file_path.push(file_name);
            relative_path_buf.push(file_name);

            // Normalize path
            let sanitized_path_str = file_path.to_string_lossy().replace('\\', "/");
            let sanitized_path = std::path::Path::new(&sanitized_path_str);

            // Relative path for config
            let relative_path_str = relative_path_buf.to_string_lossy().replace('\\', "/");
            let relative_path = std::path::PathBuf::from(relative_path_str);

            // Use sanitized path for creation
            if let Some(parent) = sanitized_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // info!("Extracting: {}", file_name); // Valid input, but too detailed for parallel log? PB shows progress.

            let mut out_file = std::fs::File::create(sanitized_path)?;

            // Thread-local VolumeManager
            let mut volume_manager = VolumeManager {
                base_dir: input_base_dir_shared.clone(),
                file_list: volume_files_shared.clone(),
                open_files: std::collections::HashMap::new(),
            };

            // Also need local DzipReader for Main Volume (ID 0)
            // But VolumeManager handles ID > 0.
            // ID 0 chunks must be read from MAIN file.
            // DzipReader::read_chunk_data_with_volumes handles this?
            // "if chunk.file == 0 { self.read_chunk_data(chunk) }"
            // So we need a DzipReader for `self`.
            let main_file = std::fs::File::open(input_path).map_err(dzip_core::DzipError::Io)?;
            let mut reader = dzip_core::reader::DzipReader::new(main_file);

            // Determine compression from the first chunk
            let mut compression = CompressionMethod::Dz; // Default
            let mut archive_index = 0;
            if let Some(&first_chunk_id) = chunk_ids.first() {
                let chunk = &chunks[first_chunk_id as usize];
                archive_index = chunk.file;

                use dzip_core::format::*;
                if (chunk.flags & CHUNK_ZLIB) != 0 {
                    compression = CompressionMethod::Zlib;
                } else if (chunk.flags & CHUNK_BZIP) != 0 {
                    compression = CompressionMethod::Bzip;
                } else if (chunk.flags & CHUNK_COPYCOMP) != 0 {
                    compression = CompressionMethod::Copy;
                } else if (chunk.flags & CHUNK_ZERO) != 0 {
                    compression = CompressionMethod::Zero;
                } else if (chunk.flags & CHUNK_MP3) != 0 {
                    compression = CompressionMethod::Mp3;
                } else if (chunk.flags & CHUNK_JPEG) != 0 {
                    compression = CompressionMethod::Jpeg;
                } else if (chunk.flags & CHUNK_LZMA) != 0 {
                    compression = CompressionMethod::Lzma;
                } else if (chunk.flags & CHUNK_DZ) != 0 {
                    compression = CompressionMethod::Dz;
                } else if (chunk.flags & CHUNK_COMBUF) != 0 {
                    compression = CompressionMethod::Combuf;
                } else if (chunk.flags & CHUNK_RANDOMACCESS) != 0 {
                    compression = CompressionMethod::RandomAccess;
                }
            }

            for &chunk_id in chunk_ids {
                let chunk = &chunks[chunk_id as usize];
                /*
                debug!(
                    "Chunk {} - Offset: {}, CompLen: {}, DecompLen: {}, File: {}, Flags: {:#x}",
                    chunk_id,
                    chunk.offset,
                    chunk.compressed_length,
                    chunk.decompressed_length,
                    chunk.file,
                    chunk.flags
                );
                */
                match reader.read_chunk_data_with_volumes(chunk, &mut volume_manager) {
                    Ok(data) => {
                        use std::io::Write;
                        out_file.write_all(&data)?;
                    }
                    Err(dzip_core::DzipError::UnsupportedCompression(flags)) => {
                        warn!(
                            "Skipping chunk {} due to unsupported compression (flags: {:#x})",
                            chunk_id, flags
                        );
                    }
                    Err(_e) => {
                        error!("Error extracting chunk {}: {}", chunk_id, _e);
                        // Continue? Or fail? Currently continue.
                        continue;
                    }
                }
            }

            Ok(config::FileEntry {
                path: relative_path,
                archive_file_index: archive_index,
                compression,
                modifiers: String::new(),
            })
        })
        .collect::<Result<Vec<config::FileEntry>>>()?;

    pack_config.files = results;

    // Write config file
    let input_name = std::path::Path::new(input_path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let config_filename = format!("{}.toml", input_name);
    let config_path = std::path::Path::new(output_dir).join(config_filename);
    let toml_string = toml::to_string_pretty(&pack_config).expect("Failed to serialize config");
    std::fs::write(config_path, toml_string)?;

    pb.finish_with_message("Unpack complete");
    info!("Unpack complete.");
    Ok(())
}
fn pack_archive(input_path: &str, output_dir: &str) -> Result<()> {
    let config_path = std::path::Path::new(input_path);
    info!("Parsing config file: {}", config_path.display());
    let mut config = config::parse_config(config_path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // If base_dir is "." (default), make it relative to the config file's directory
    if config.base_dir == std::path::Path::new(".") {
        if let Some(parent) = config_path.parent() {
            if !parent.as_os_str().is_empty() {
                config.base_dir = parent.to_path_buf();
            }
        }
    }

    std::fs::create_dir_all(output_dir)?;

    use dzip_core::format::*;
    use std::io::{Seek, SeekFrom, Write};

    // --- Prepare Metadata ---
    // 1. Strings: User Files + Unique Directories
    // Note: Dzip strings table contains filenames (basename) and directory paths.
    // The exact structure is: [List of User Filenames], [List of Directory Paths].
    // Wait, the format in unpacking:
    // strings = reader.read_strings(settings.num_user_files + settings.num_directories - 1)?
    // And map points to dir_index.
    // So strings table is: [file1_name, file2_name, ..., dir1_path, dir2_path, ...].
    // Note root dir is implicit/empty and usually not in strings table?
    // Unpacker: `dir_index = num_user_files + (dir_id - 1)`.
    // If dir_id=1, index = num_user_files.
    // So yes, strings list is [Files..., Dir1, Dir2...].

    // Collect File Names
    let mut file_names = Vec::new();
    for entry in &config.files {
        // Use filename component
        if let Some(name) = entry.path.file_name() {
            file_names.push(name.to_string_lossy().to_string());
        } else {
            return Err(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid file path").into(),
            );
        }
    }

    // Collect Unique Directories and assign IDs
    let mut directories = Vec::new();
    let mut dir_map = std::collections::HashMap::new(); // path -> dir_id (1-based)

    // Directory ID 0 is Root.
    // We need to map each file to a dir_id.
    let mut file_dir_ids = Vec::new();

    for entry in &config.files {
        let parent = entry.path.parent().unwrap_or(std::path::Path::new(""));
        // Force Windows-style backslashes as requested
        let parent_str = parent.to_string_lossy().replace('/', "\\");

        if parent_str.is_empty() || parent_str == "." {
            file_dir_ids.push(0u16);
        } else {
            // Check if known
            if let Some(&id) = dir_map.get(&parent_str) {
                file_dir_ids.push(id);
            } else {
                // New directory
                // Directories list stores paths.
                directories.push(parent_str.clone());
                let id = directories.len() as u16; // 1-based
                dir_map.insert(parent_str, id);
                file_dir_ids.push(id);
            }
        }
    }

    let num_user_files = file_names.len() as u16;
    let num_directories = (directories.len() + 1) as u16; // +1 for Root?
    // Unpacker: `strings_count = num_user_files + num_directories - 1`.
    // So strings count = files + dirs.
    // Strings array = [Files..., Dirs...].
    // Root dir is NOT in strings.

    let mut all_strings = file_names;
    all_strings.extend(directories);

    // --- Open Volumes ---
    if config.archives.is_empty() {
        return Err(
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "No archives specified").into(),
        );
    }

    let mut writers = std::collections::HashMap::new();
    for (i, name) in config.archives.iter().enumerate() {
        let path = std::path::Path::new(output_dir).join(name);
        info!("Opening volume {}: {}", i, path.display());
        let f = std::fs::File::create(&path)?;
        writers.insert(i as u16, f);
    }

    // --- Pre-calculate Header Size (Volume 0) ---
    // Header (ArchiveSettings) = 4+2+2+1 = 9
    // Strings = Sum(len+1)
    // FileMap (ChunkMap) = NumFiles * (2 + NumChunksInFile*2 + 2)
    // ChunkSettings = 2+2=4
    // ChunkTable = NumChunks * 16
    // Auxiliary File List = Sum(len+1) of archives[1..]

    // Assuming 1 chunk per file
    let num_chunks = num_user_files;

    let mut header_size = 9;
    for s in &all_strings {
        header_size += s.len() as u64 + 1;
    }
    let file_map_size = (num_user_files as u64) * 6; // DirID(2) + ChunkID(2) + Term(2)
    header_size += file_map_size;

    header_size += 4; // ChunkSettings
    let chunk_table_size = (num_chunks as u64) * 16;
    header_size += chunk_table_size;

    // Add Volume List Size
    if config.archives.len() > 1 {
        for name in &config.archives[1..] {
            header_size += name.len() as u64 + 1;
        }
    }

    // Should we add GlobalSettings size? Only if we use DZ compression.
    // Config options might specify usage. For now assume minimal header.
    // We will update this offset if needed.

    // Seek Volume 0
    if let Some(w) = writers.get_mut(&0) {
        w.seek(SeekFrom::Start(header_size))?;
    }

    // --- Process Files and Write Chunks ---
    let mut chunks = Vec::new();
    let mut chunk_map = Vec::new(); // (dir_id, vec![chunk_id])

    // Parallel Compression Phase
    info!("Compressing chunks in parallel...");
    let pb = ProgressBar::new(config.files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    let processed_files: Vec<(u16, Vec<u8>, usize, u16)> = config
        .files
        .par_iter()
        .enumerate()
        .map(|(i, entry)| {
            let full_path = config.base_dir.join(&entry.path);
            debug!("Processing file {}: {}", i, full_path.display());
            pb.set_message(format!("Compressing {}", entry.path.display()));

            let raw_data = std::fs::read(&full_path).map_err(|e| {
                dzip_core::DzipError::Io(std::io::Error::other(format!(
                    "Failed to read {}: {}",
                    full_path.display(),
                    e
                )))
            })?;
            let original_len = raw_data.len();

            let method = entry.compression;
            let (flags, compressed_data) = compress_data(&raw_data, method)?;

            pb.inc(1);
            Ok((
                entry.archive_file_index,
                compressed_data,
                original_len,
                flags,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    pb.finish_with_message("Compression complete");

    // Sequential Write Phase
    info!("Writing compressed chunks to volumes...");
    for (i, (archive_id, compressed_data, original_len, flags)) in
        processed_files.into_iter().enumerate()
    {
        let chunk_id = chunks.len() as u16;

        let writer = writers.get_mut(&archive_id).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Archive volume {} not found in config", archive_id),
            )
        })?;

        let offset = writer.stream_position()? as u32;
        writer.write_all(&compressed_data)?;

        chunks.push(Chunk {
            offset,
            compressed_length: compressed_data.len() as u32,
            decompressed_length: original_len as u32,
            flags,
            file: archive_id,
        });

        chunk_map.push((file_dir_ids[i], vec![chunk_id]));
    }

    // --- Write Header ---
    info!("Writing header to Volume 0...");
    let main_writer = writers
        .get_mut(&0)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Volume 0 missing"))?;

    main_writer.seek(SeekFrom::Start(0))?;

    // We need DzipWriter
    struct SimpleWriter<'a, W: Write + Seek>(&'a mut W);
    impl<'a, W: Write + Seek> Write for SimpleWriter<'a, W> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }
    impl<'a, W: Write + Seek> Seek for SimpleWriter<'a, W> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(pos)
        }
    }

    let mut dzip_writer = dzip_core::writer::DzipWriter::new(SimpleWriter(main_writer));

    // ... rest of header writing ...

    dzip_writer.write_archive_settings(&ArchiveSettings {
        header: 0x5A525444, // DTRZ
        num_user_files,
        num_directories,
        version: 0,
    })?;

    // ...

    dzip_writer.write_strings(&all_strings)?;
    dzip_writer.write_file_chunk_map(&chunk_map)?;

    // ...

    let num_archive_files = config.archives.len() as u16;

    dzip_writer.write_chunk_settings(&ChunkSettings {
        num_archive_files,
        num_chunks: chunks.len() as u16,
    })?;

    dzip_writer.write_chunks(&chunks)?;

    // Write Auxiliary File List
    if config.archives.len() > 1 {
        let aux_files = &config.archives[1..];
        dzip_writer.write_strings(aux_files)?;
    }

    let has_dz = chunks.iter().any(|c| (c.flags & CHUNK_DZ) != 0);
    if has_dz {
        dzip_writer.write_global_settings(&RangeSettings {
            win_size: 0,
            flags: 0,
            offset_table_size: 0,
            offset_tables: 0,
            offset_contexts: 0,
            ref_length_table_size: 0,
            ref_length_tables: 0,
            ref_offset_table_size: 0,
            ref_offset_tables: 0,
            big_min_match: 0,
        })?;
    }

    info!("Pack complete.");
    Ok(())
}

fn verify_archive(input_path: &str) -> Result<()> {
    use dzip_core::format::*;

    let mut reader = dzip_core::reader::DzipReader::new(
        std::fs::File::open(input_path).map_err(dzip_core::DzipError::Io)?,
    );

    let settings = reader.read_archive_settings()?;

    // Read strings (filenames + dirnames)
    // Formula: num_user_files + num_directories - 1
    let strings_count = settings.num_user_files as usize + settings.num_directories as usize - 1;
    let strings = reader.read_strings(strings_count)?;

    // Read FileChunkMap
    let map = reader.read_file_chunk_map(settings.num_user_files as usize)?;

    // We need chunk headers to get sizes
    let chunk_settings = reader.read_chunk_settings()?;
    let mut chunks = reader.read_chunks(chunk_settings.num_chunks as usize)?;

    // Read Auxiliary Files (Volumes)
    let num_volumes_expected = chunk_settings.num_archive_files.saturating_sub(1);
    let volume_files = if num_volumes_expected > 0 {
        reader.read_strings(num_volumes_expected as usize)?
    } else {
        Vec::new()
    };

    // Prepare shared data for VolumeManager
    let input_base_dir = std::path::Path::new(input_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let input_base_dir_shared = input_base_dir.to_path_buf();
    let volume_files_shared = volume_files.clone();

    // Define VolumeManager (Same as unpack)
    struct VolumeManager {
        base_dir: std::path::PathBuf,
        file_list: Vec<String>,
        open_files: std::collections::HashMap<u16, std::fs::File>,
    }

    impl dzip_core::reader::VolumeSource for VolumeManager {
        fn open_volume(&mut self, id: u16) -> Result<&mut dyn dzip_core::reader::ReadSeek> {
            use std::collections::hash_map::Entry;

            if id == 0 {
                return Err(dzip_core::DzipError::Io(std::io::Error::other(
                    "Volume ID 0 is reserved for main file",
                )));
            }

            let list_index = (id - 1) as usize;
            if list_index >= self.file_list.len() {
                return Err(dzip_core::DzipError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Volume ID {} not found in file list", id),
                )));
            }

            match self.open_files.entry(id) {
                Entry::Occupied(e) => Ok(e.into_mut()),
                Entry::Vacant(e) => {
                    let file_name = &self.file_list[list_index];
                    let path = self.base_dir.join(file_name);
                    // debug!("Opening volume {}: {}", id, path.display());
                    let file = std::fs::File::open(&path)?;
                    Ok(e.insert(file))
                }
            }
        }
    }

    // --- Chunk Size Correction (Same as unpack) ---
    let mut file_sizes = std::collections::HashMap::new();
    if let Ok(meta) = std::fs::metadata(input_path) {
        file_sizes.insert(0u16, meta.len());
    }
    for (i, vol_name) in volume_files.iter().enumerate() {
        let path = input_base_dir.join(vol_name);
        if let Ok(meta) = std::fs::metadata(&path) {
            file_sizes.insert((i + 1) as u16, meta.len());
        }
    }

    let mut chunks_by_file: std::collections::HashMap<u16, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, chunk) in chunks.iter().enumerate() {
        chunks_by_file.entry(chunk.file).or_default().push(i);
    }

    for (file_id, mut indices) in chunks_by_file {
        indices.sort_by_key(|&i| chunks[i].offset);

        let file_size = *file_sizes.get(&file_id).unwrap_or(&0);

        for i in 0..indices.len() {
            let idx = indices[i];
            let chunk_offset = chunks[idx].offset as u64;

            let limit = if i + 1 < indices.len() {
                chunks[indices[i + 1]].offset as u64
            } else {
                file_size
            };

            let available = limit.saturating_sub(chunk_offset);

            use dzip_core::format::{CHUNK_BZIP, CHUNK_DZ, CHUNK_LZMA, CHUNK_ZLIB};
            let is_compressed =
                (chunks[idx].flags & (CHUNK_LZMA | CHUNK_ZLIB | CHUNK_BZIP | CHUNK_DZ)) != 0;
            let equal_sizes = chunks[idx].compressed_length == chunks[idx].decompressed_length;

            if is_compressed && equal_sizes {
                if chunks[idx].compressed_length != available as u32 {
                    debug!(
                        "Correcting Equal-Size Chunk {} from {} to {} (File {}, Offset {})",
                        idx,
                        chunks[idx].compressed_length,
                        available,
                        chunks[idx].file,
                        chunk_offset
                    );
                    chunks[idx].compressed_length = available as u32;
                }
            } else if (chunks[idx].compressed_length as u64) > available {
                debug!(
                    "Correcting Chunk {} size from {} to {} (File {}, Offset {})",
                    idx, chunks[idx].compressed_length, available, chunks[idx].file, chunk_offset
                );
                chunks[idx].compressed_length = available as u32;
            }
        }
    }

    println!("Verifying archive integrity...");

    println!(
        "{:<5} | {:<7} | {:<10} | {:<10} | {:<8} | Path",
        "Idx", "Status", "Size", "Packed", "Method"
    );
    println!(
        "{:-<5}-+-{:-<7}-+-{:-<10}-+-{:-<10}-+-{:-<8}-+-{:-<20}",
        "", "", "", "", "", ""
    );

    // Use parallel iterator to verify
    // We need to collect results to print them in order (or we could print as we go if we didn't care about order, but table looks best ordered)
    // Order is important for "Idx".

    let results: Vec<String> = map
        .par_iter()
        .enumerate()
        .map(|(i, (dir_id, chunk_ids))| -> Result<String> {
            let file_name = &strings[i];

            // Reconstruct path
            let mut full_path = String::new();
            if *dir_id > 0 {
                let dir_index = settings.num_user_files as usize + (*dir_id as usize) - 1;
                if let Some(dir_name) = strings.get(dir_index) {
                    full_path.push_str(dir_name);
                    if !full_path.ends_with('/') && !full_path.ends_with('\\') {
                        full_path.push('/');
                    }
                }
            }
            full_path.push_str(file_name);

            // Calculate sizes
            let mut size = 0;
            let mut packed = 0;
            let mut method_str = "Unknown";

            if let Some(&first_chunk_id) = chunk_ids.first() {
                let chunk = &chunks[first_chunk_id as usize];
                // Determine method from first chunk
                if (chunk.flags & CHUNK_ZLIB) != 0 {
                    method_str = "Zlib";
                } else if (chunk.flags & CHUNK_BZIP) != 0 {
                    method_str = "Bzip";
                } else if (chunk.flags & CHUNK_LZMA) != 0 {
                    method_str = "LZMA";
                } else if (chunk.flags & CHUNK_COPYCOMP) != 0 {
                    method_str = "Copy";
                } else if (chunk.flags & CHUNK_ZERO) != 0 {
                    method_str = "Zero";
                } else if (chunk.flags & CHUNK_DZ) != 0 {
                    method_str = "Dz";
                }
            }

            // Verify integrity
            // We need a local DzipReader and VolumeManager
            let main_file = std::fs::File::open(input_path).map_err(dzip_core::DzipError::Io)?;
            let mut local_reader = dzip_core::reader::DzipReader::new(main_file);

            let mut volume_manager = VolumeManager {
                base_dir: input_base_dir_shared.clone(),
                file_list: volume_files_shared.clone(),
                open_files: std::collections::HashMap::new(),
            };

            let mut chunk_status = "OK";
            for &chunk_id in chunk_ids {
                if let Some(chunk) = chunks.get(chunk_id as usize) {
                    if let Err(_e) =
                        local_reader.read_chunk_data_with_volumes(chunk, &mut volume_manager)
                    {
                        // Log error but return FAIL string
                        error!("Chunk {} failed verification: {}", chunk_id, _e);
                        chunk_status = "FAIL";
                    }
                } else {
                    chunk_status = "FAIL";
                }
            }
            let status = chunk_status;

            for &cid in chunk_ids {
                let chunk = &chunks[cid as usize];
                size += chunk.decompressed_length;
                packed += chunk.compressed_length;
            }

            Ok(format!(
                "{:<5} | {:<7} | {:<10} | {:<10} | {:<8} | {}",
                i, status, size, packed, method_str, full_path
            ))
        })
        .collect::<Result<Vec<String>>>()?;

    for line in results {
        println!("{}", line);
    }

    Ok(())
}
