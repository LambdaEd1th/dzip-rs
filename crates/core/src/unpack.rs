use binrw::{BinRead, NullString};
use log::{info, warn};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::codec::decompress;
use crate::error::DzipError;
use crate::format::{
    ArchiveHeader, CURRENT_DIR_STR, ChunkDiskEntry, ChunkFlags, ChunkTableHeader,
    DEFAULT_BUFFER_SIZE, FileMapDiskEntry, RangeSettingsDisk,
};
use crate::io::{ReadSeekSend, UnpackSink, UnpackSource};
use crate::model::{ArchiveMeta, ChunkDef, Config, FileEntry, RangeSettings};
use crate::utils::{decode_flags, to_native_path};

#[derive(Debug)]
pub struct ArchiveMetadata {
    pub version: u8,
    pub user_files: Vec<String>,
    pub directories: Vec<String>,
    pub map_entries: Vec<FileMapDiskEntry>,
    pub raw_chunks: Vec<RawChunk>,
    pub split_file_names: Vec<String>,
    pub range_settings: Option<RangeSettings>,
    pub main_file_len: u64,
}

pub struct UnpackPlan {
    pub metadata: ArchiveMetadata,
    pub processed_chunks: Vec<RawChunk>,
}

#[derive(Clone, Debug)]
pub struct RawChunk {
    pub id: u16,
    pub offset: u32,
    pub _head_c_len: u32,
    pub d_len: u32,
    pub flags: u16,
    pub file_idx: u16,
    pub real_c_len: u32,
}

pub fn do_unpack(
    source: &dyn UnpackSource,
    sink: &dyn UnpackSink,
    keep_raw: bool,
    on_progress: impl Fn(crate::ProgressEvent) + Send + Sync,
) -> Result<Config> {
    let meta = ArchiveMetadata::load(source)?;
    let plan = UnpackPlan::build(meta, source)?;
    plan.extract(sink, keep_raw, source, on_progress)?;
    let config = plan.generate_config_struct()?;
    info!("Unpack complete. Config object generated.");
    Ok(config)
}

impl ArchiveMetadata {
    pub fn load(source: &dyn UnpackSource) -> Result<Self> {
        let mut main_file_raw = source.open_main()?;
        let main_file_len = main_file_raw
            .seek(SeekFrom::End(0))
            .map_err(DzipError::Io)?;
        main_file_raw
            .seek(SeekFrom::Start(0))
            .map_err(DzipError::Io)?;

        let mut reader = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, main_file_raw);

        // 1. Read Header
        let header = ArchiveHeader::read(&mut reader)
            .map_err(|e| DzipError::Generic(format!("Failed to read header: {}", e)))?;

        info!(
            "Header: Ver {}, Files {}, Dirs {}",
            header.version, header.num_files, header.num_dirs
        );

        // 2. Read File Names
        let file_names_raw: Vec<NullString> = Vec::read_args(
            &mut reader,
            binrw::VecArgs {
                count: header.num_files as usize,
                inner: (),
            },
        )
        .map_err(|e| DzipError::Generic(format!("Failed to read filenames: {}", e)))?;

        let user_files: Vec<String> = file_names_raw.into_iter().map(|s| s.to_string()).collect();

        // 3. Read Directories
        let mut directories = Vec::with_capacity(header.num_dirs as usize);
        directories.push(CURRENT_DIR_STR.to_string());

        if header.num_dirs > 1 {
            let dir_names_raw: Vec<NullString> = Vec::read_args(
                &mut reader,
                binrw::VecArgs {
                    count: (header.num_dirs - 1) as usize,
                    inner: (),
                },
            )
            .map_err(|e| DzipError::Generic(format!("Failed to read directories: {}", e)))?;

            for d in dir_names_raw {
                directories.push(d.to_string());
            }
        }

        // 4. Read File Maps
        let map_entries: Vec<FileMapDiskEntry> = Vec::read_args(
            &mut reader,
            binrw::VecArgs {
                count: header.num_files as usize,
                inner: (),
            },
        )
        .map_err(|e| DzipError::Generic(format!("Failed to read file maps: {}", e)))?;

        // 5. Read Chunk Table Header
        let chunk_header = ChunkTableHeader::read(&mut reader)
            .map_err(|e| DzipError::Generic(format!("Failed to read chunk table header: {}", e)))?;

        info!(
            "Chunk Settings: {} chunks in {} archive files",
            chunk_header.num_chunks, chunk_header.num_arch_files
        );

        // 6. Read Chunk Definitions
        let disk_chunks: Vec<ChunkDiskEntry> = Vec::read_args(
            &mut reader,
            binrw::VecArgs {
                count: chunk_header.num_chunks as usize,
                inner: (),
            },
        )
        .map_err(|e| DzipError::Generic(format!("Failed to read chunk entries: {}", e)))?;

        let mut raw_chunks = Vec::with_capacity(disk_chunks.len());
        let mut has_dz_chunk = false;

        for (i, c) in disk_chunks.into_iter().enumerate() {
            let flags = ChunkFlags::from_bits_truncate(c.flags);
            if flags.contains(ChunkFlags::DZ_RANGE) {
                has_dz_chunk = true;
            }
            raw_chunks.push(RawChunk {
                id: i as u16,
                offset: c.offset,
                _head_c_len: c.c_len,
                d_len: c.d_len,
                flags: c.flags,
                file_idx: c.file_idx,
                real_c_len: 0,
            });
        }

        // 7. Read Split Filenames
        let mut split_file_names = Vec::new();
        if chunk_header.num_arch_files > 1 {
            info!(
                "Reading {} split archive filenames...",
                chunk_header.num_arch_files - 1
            );
            let splits_raw: Vec<NullString> = Vec::read_args(
                &mut reader,
                binrw::VecArgs {
                    count: (chunk_header.num_arch_files - 1) as usize,
                    inner: (),
                },
            )
            .map_err(|e| DzipError::Generic(format!("Failed to read split filenames: {}", e)))?;

            for s in splits_raw {
                split_file_names.push(s.to_string());
            }
        }

        // 8. Read Range Settings
        let range_settings = if has_dz_chunk {
            info!("Detected CHUNK_DZ, reading RangeSettings...");
            let rs_disk = RangeSettingsDisk::read(&mut reader)
                .map_err(|e| DzipError::Generic(format!("Failed to read RangeSettings: {}", e)))?;

            Some(RangeSettings {
                win_size: rs_disk.win_size,
                flags: rs_disk.flags,
                offset_table_size: rs_disk.offset_table_size,
                offset_tables: rs_disk.offset_tables,
                offset_contexts: rs_disk.offset_contexts,
                ref_length_table_size: rs_disk.ref_length_table_size,
                ref_length_tables: rs_disk.ref_length_tables,
                ref_offset_table_size: rs_disk.ref_offset_table_size,
                ref_offset_tables: rs_disk.ref_offset_tables,
                big_min_match: rs_disk.big_min_match,
            })
        } else {
            None
        };

        Ok(Self {
            version: header.version,
            user_files,
            directories,
            map_entries,
            raw_chunks,
            split_file_names,
            range_settings,
            main_file_len,
        })
    }
}

impl UnpackPlan {
    pub fn build(metadata: ArchiveMetadata, source: &dyn UnpackSource) -> Result<Self> {
        let processed_chunks = Self::calculate_chunk_sizes(&metadata, source)?;
        Ok(Self {
            metadata,
            processed_chunks,
        })
    }

    fn calculate_chunk_sizes(
        meta: &ArchiveMetadata,
        source: &dyn UnpackSource,
    ) -> Result<Vec<RawChunk>> {
        let mut chunks = meta.raw_chunks.clone();
        let mut file_chunks_map: HashMap<u16, Vec<usize>> = HashMap::new();
        for (idx, c) in chunks.iter().enumerate() {
            file_chunks_map.entry(c.file_idx).or_default().push(idx);
        }

        for (f_idx, c_indices) in file_chunks_map.iter() {
            let mut sorted_indices = c_indices.clone();
            sorted_indices.sort_by_key(|&i| chunks[i].offset);

            let current_file_size = if *f_idx == 0 {
                meta.main_file_len
            } else {
                let idx = (*f_idx - 1) as usize;
                let split_name = meta.split_file_names.get(idx).ok_or_else(|| {
                    DzipError::Generic(format!("Invalid split file index {} in header", f_idx))
                })?;
                source.get_split_len(split_name)?
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
        Ok(chunks)
    }

    pub fn extract(
        &self,
        sink: &dyn UnpackSink,
        keep_raw: bool,
        source: &dyn UnpackSource,
        on_progress: impl Fn(crate::ProgressEvent) + Send + Sync,
    ) -> Result<()> {
        info!("Extracting {} files...", self.metadata.map_entries.len());
        on_progress(crate::ProgressEvent::Start(self.metadata.map_entries.len()));
        let chunk_indices: HashMap<u16, usize> = self
            .processed_chunks
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        // Fixed: Use enumerate to get the file index, as 'id' is removed from struct
        self.metadata
            .map_entries
            .par_iter()
            .enumerate()
            .try_for_each_init(
                HashMap::new,
                |file_cache: &mut HashMap<u16, Box<dyn ReadSeekSend>>,
                 (file_id, entry)|
                 -> Result<()> {
                    // Fixed: Use 'file_id' index
                    let fname = &self.metadata.user_files[file_id];

                    // Fixed: Cast u16 dir_idx to usize
                    let raw_dir = if (entry.dir_idx as usize) < self.metadata.directories.len() {
                        &self.metadata.directories[entry.dir_idx as usize]
                    } else {
                        CURRENT_DIR_STR
                    };

                    let mut path_buf = PathBuf::from(raw_dir);
                    if raw_dir != CURRENT_DIR_STR && !raw_dir.is_empty() {
                        path_buf.push(fname);
                    } else {
                        path_buf = PathBuf::from(fname);
                    }

                    let rel_path = to_native_path(&path_buf);

                    if let Some(parent) = path_buf
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty() && p.as_os_str() != ".")
                    {
                        sink.create_dir_all(&to_native_path(parent))?;
                    }

                    let out_file = sink.create_file(&rel_path)?;
                    let mut writer = BufWriter::with_capacity(DEFAULT_BUFFER_SIZE, out_file);

                    for cid in &entry.chunk_ids {
                        if let Some(&idx) = chunk_indices.get(cid) {
                            let chunk = &self.processed_chunks[idx];
                            let source_file = match file_cache.entry(chunk.file_idx) {
                                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                                std::collections::hash_map::Entry::Vacant(e) => {
                                    let f = if chunk.file_idx == 0 {
                                        source.open_main()?
                                    } else {
                                        let split_idx = (chunk.file_idx - 1) as usize;
                                        let split_name = self
                                            .metadata
                                            .split_file_names
                                            .get(split_idx)
                                            .ok_or_else(|| {
                                                DzipError::Generic(format!(
                                                    "Invalid archive file index {} for chunk {}",
                                                    chunk.file_idx, chunk.id
                                                ))
                                            })?;
                                        source.open_split(split_name)?
                                    };
                                    e.insert(f)
                                }
                            };

                            source_file
                                .seek(SeekFrom::Start(chunk.offset as u64))
                                .map_err(DzipError::Io)?;

                            let mut source_reader =
                                BufReader::with_capacity(DEFAULT_BUFFER_SIZE, source_file)
                                    .take(chunk.real_c_len as u64);

                            if let Err(e) = decompress(
                                &mut source_reader,
                                &mut writer,
                                chunk.flags,
                                chunk.d_len,
                            ) {
                                if keep_raw {
                                    let err_msg = e.to_string();
                                    let mut raw_buf_reader = source_reader.into_inner();
                                    raw_buf_reader
                                        .seek(SeekFrom::Start(chunk.offset as u64))
                                        .map_err(DzipError::Io)?;
                                    let mut raw_take = raw_buf_reader.take(chunk.real_c_len as u64);
                                    warn!(
                                        "Failed to decompress chunk {}: {}. Writing raw data.",
                                        chunk.id, err_msg
                                    );
                                    std::io::copy(&mut raw_take, &mut writer)
                                        .map_err(DzipError::Io)?;
                                } else {
                                    return Err(e);
                                }
                            }
                        }
                    }
                    writer.flush().map_err(DzipError::Io)?;
                    on_progress(crate::ProgressEvent::Inc(1));
                    Ok(())
                },
            )?;
        on_progress(crate::ProgressEvent::Finish);
        Ok(())
    }

    pub fn generate_config_struct(&self) -> Result<Config> {
        let mut config_files = Vec::new();

        for (i, entry) in self.metadata.map_entries.iter().enumerate() {
            let fname = &self.metadata.user_files[i];

            let raw_dir = if (entry.dir_idx as usize) < self.metadata.directories.len() {
                &self.metadata.directories[entry.dir_idx as usize]
            } else {
                CURRENT_DIR_STR
            };
            let mut path_buf = PathBuf::from(raw_dir);
            if raw_dir != CURRENT_DIR_STR && !raw_dir.is_empty() {
                path_buf.push(fname);
            } else {
                path_buf = PathBuf::from(fname);
            }

            let full_raw_path = to_native_path(&path_buf);
            let normalized_dir = to_native_path(Path::new(raw_dir));
            let chunk_id = *entry.chunk_ids.first().unwrap_or(&0);

            config_files.push(FileEntry {
                path: full_raw_path,
                directory: normalized_dir,
                filename: fname.clone(),
                chunk: chunk_id,
            });
        }

        let mut config_chunks = Vec::new();
        let mut sorted_chunks = self.processed_chunks.clone();
        sorted_chunks.sort_by_key(|c| c.id);

        for c in sorted_chunks {
            let flags_vec = decode_flags(c.flags);
            let flag_str = flags_vec.first().map(|s| s.to_string()).unwrap_or_default();

            config_chunks.push(ChunkDef {
                id: c.id,
                offset: c.offset,
                size_compressed: c.real_c_len,
                size_decompressed: c.d_len,
                flag: flag_str,
                archive_file_index: c.file_idx,
            });
        }

        Ok(Config {
            archive: ArchiveMeta {
                version: self.metadata.version,
                total_files: self.metadata.map_entries.len() as u16,
                total_directories: self.metadata.directories.len() as u16,
                total_chunks: self.processed_chunks.len() as u16,
            },
            archive_files: self.metadata.split_file_names.clone(),
            range_settings: self.metadata.range_settings.clone(),
            files: config_files,
            chunks: config_chunks,
        })
    }
}
