use dzip_core::reader::DzipReader;
use std::fs::File;
use std::path::PathBuf;

#[test]
fn test_real_file_parsing() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../test_data/ExampleSingleArchive/test1.dz");

    if !path.exists() {
        eprintln!("Test file not found at {:?}, skipping.", path);
        return;
    }

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = DzipReader::new(file);

    let settings = reader
        .read_archive_settings()
        .expect("Failed to read archive settings");

    assert_eq!(settings.header, 0x5A525444);
    assert!(settings.num_user_files > 0);

    // Note: The first directory is root and has no string entry.
    let strings_count = (settings.num_user_files + settings.num_directories - 1) as usize;
    let strings = reader
        .read_strings(strings_count)
        .expect("Failed to read strings");

    // For test1.dz, we expect 2 user files and 1 directory (root, skipped).
    // So read 2 strings.
    assert_eq!(strings.len(), strings_count);

    let map = reader
        .read_file_chunk_map(settings.num_user_files as usize)
        .expect("Failed to read file/chunk map");
    assert_eq!(map.len(), settings.num_user_files as usize);

    let chunk_settings = reader
        .read_chunk_settings()
        .expect("Failed to read chunk settings");

    let chunks = reader
        .read_chunks(chunk_settings.num_chunks as usize)
        .expect("Failed to read chunks");
    assert_eq!(chunks.len(), chunk_settings.num_chunks as usize);

    let num_archive_files = chunk_settings.num_archive_files;
    if num_archive_files > 1 {
        let file_list = reader
            .read_file_list((num_archive_files - 1) as usize)
            .expect("Failed to read file list");
        assert_eq!(file_list.len(), (num_archive_files - 1) as usize);
    }

    let _global_settings = reader
        .read_global_settings()
        .expect("Failed to read global settings");
}

#[test]
fn test_real_file_parsing_2() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../test_data/ExampleSingleArchive/test2.dz");

    if !path.exists() {
        eprintln!("Test file not found at {:?}, skipping.", path);
        return;
    }

    let file = File::open(path).expect("Failed to open file");
    let mut reader = DzipReader::new(file);

    let settings = reader
        .read_archive_settings()
        .expect("Failed to read archive settings");

    assert_eq!(settings.header, 0x5A525444);

    let strings_count = (settings.num_user_files + settings.num_directories - 1) as usize;
    let strings = reader
        .read_strings(strings_count)
        .expect("Failed to read strings");
    assert_eq!(strings.len(), strings_count);

    let map = reader
        .read_file_chunk_map(settings.num_user_files as usize)
        .expect("Failed to read file/chunk map");
    assert_eq!(map.len(), settings.num_user_files as usize);

    let chunk_settings = reader
        .read_chunk_settings()
        .expect("Failed to read chunk settings");

    let chunks = reader
        .read_chunks(chunk_settings.num_chunks as usize)
        .expect("Failed to read chunks");
    assert_eq!(chunks.len(), chunk_settings.num_chunks as usize);

    let num_archive_files = chunk_settings.num_archive_files;
    if num_archive_files > 1 {
        let file_list = reader
            .read_file_list((num_archive_files - 1) as usize)
            .expect("Failed to read file list");
        assert_eq!(file_list.len(), (num_archive_files - 1) as usize);
    }

    let _global_settings = reader
        .read_global_settings()
        .expect("Failed to read global settings");
}

#[test]
fn test_split_archive_parsing() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../test_data/ExampleSplitArchive/testnew.dz");

    if !path.exists() {
        eprintln!("Test file not found at {:?}, skipping.", path);
        return;
    }

    let file = File::open(path).expect("Failed to open file");
    let mut reader = DzipReader::new(file);

    let settings = reader
        .read_archive_settings()
        .expect("Failed to read archive settings");

    assert_eq!(settings.header, 0x5A525444);

    let strings_count = (settings.num_user_files + settings.num_directories - 1) as usize;
    let _strings = reader
        .read_strings(strings_count)
        .expect("Failed to read strings");

    let _map = reader
        .read_file_chunk_map(settings.num_user_files as usize)
        .expect("Failed to read file/chunk map");

    let chunk_settings = reader
        .read_chunk_settings()
        .expect("Failed to read chunk settings");

    // Expect 4 volumes (testnew.dz + 3 others)
    assert_eq!(chunk_settings.num_archive_files, 4);

    let chunks = reader
        .read_chunks(chunk_settings.num_chunks as usize)
        .expect("Failed to read chunks");
    assert_eq!(chunks.len(), chunk_settings.num_chunks as usize);

    if chunk_settings.num_archive_files > 1 {
        let file_list = reader
            .read_file_list((chunk_settings.num_archive_files - 1) as usize)
            .expect("Failed to read file list");
        assert_eq!(file_list.len(), 3);
        // Verify names if needed, usually they are "testnew1.dz", etc.
        println!("Split Archive Volumes: {:?}", file_list);
    }
}
