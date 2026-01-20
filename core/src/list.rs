use log::info;
use std::collections::HashMap;
use std::path::MAIN_SEPARATOR_STR;

use crate::Result;
use crate::format::CURRENT_DIR_STR;
use crate::io::UnpackSource;
use crate::unpack::ArchiveMetadata;

pub struct ListEntry {
    pub path: String,
    pub original_size: u64,
    pub chunk_count: usize,
}

pub fn do_list(source: &dyn UnpackSource) -> Result<Vec<ListEntry>> {
    info!("Reading archive header...");
    let meta = ArchiveMetadata::load(source)?;
    info!(
        "Archive loaded. Version: {}, Files: {}, Dirs: {}",
        meta.version,
        meta.map_entries.len(),
        meta.directories.len()
    );
    let chunk_lookup: HashMap<u16, &crate::unpack::RawChunk> =
        meta.raw_chunks.iter().map(|c| (c.id, c)).collect();

    let mut entries = Vec::with_capacity(meta.map_entries.len());

    for (file_id, entry) in meta.map_entries.iter().enumerate() {
        let fname = &meta.user_files[file_id];

        let raw_dir = if (entry.dir_idx as usize) < meta.directories.len() {
            &meta.directories[entry.dir_idx as usize]
        } else {
            CURRENT_DIR_STR
        };

        let full_path = if raw_dir == CURRENT_DIR_STR || raw_dir.is_empty() {
            fname.clone()
        } else {
            format!("{}{}{}", raw_dir, MAIN_SEPARATOR_STR, fname)
        };

        let mut total_size: u64 = 0;
        for cid in &entry.chunk_ids {
            if let Some(chunk) = chunk_lookup.get(cid) {
                total_size += chunk.d_len as u64;
            }
        }
        entries.push(ListEntry {
            path: full_path,
            original_size: total_size,
            chunk_count: entry.chunk_ids.len(),
        });
    }
    Ok(entries)
}
