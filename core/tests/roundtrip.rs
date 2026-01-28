use dzip_core::format::*;
use dzip_core::reader::DzipReader;
use dzip_core::writer::DzipWriter;
use std::io::Cursor;

#[test]
fn test_roundtrip() {
    let mut buffer = Vec::new();
    let archive_settings = ArchiveSettings {
        header: 0x5A525444,
        num_user_files: 2,
        num_directories: 1,
        version: 0,
    };
    let strings = vec![
        "file1.txt".to_string(),
        "file2.txt".to_string(),
        "dir1".to_string(),
    ];
    let map = vec![(0, vec![0]), (0, vec![1])];
    let chunk_settings = ChunkSettings {
        num_archive_files: 2, // Means 1 file in file list
        num_chunks: 2,
    };
    let chunks = vec![
        Chunk {
            offset: 0,
            compressed_length: 10,
            decompressed_length: 10,
            flags: 0,
            file: 0,
        },
        Chunk {
            offset: 10,
            compressed_length: 20,
            decompressed_length: 20,
            flags: 0,
            file: 0,
        },
    ];
    let file_list = vec!["archive.dzip".to_string()];
    let global_settings = RangeSettings {
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
    };

    // Pack
    {
        let mut writer = DzipWriter::new(Cursor::new(&mut buffer));
        writer.write_archive_settings(&archive_settings).unwrap();
        writer.write_strings(&strings).unwrap();
        writer.write_file_chunk_map(&map).unwrap();
        writer.write_chunk_settings(&chunk_settings).unwrap();
        writer.write_chunks(&chunks).unwrap();
        writer.write_strings(&file_list).unwrap(); // File list is just strings
        writer.write_global_settings(&global_settings).unwrap();
    }

    // Unpack
    let mut reader = DzipReader::new(Cursor::new(&buffer));
    let read_archive_settings = reader.read_archive_settings().unwrap();
    assert_eq!(archive_settings, read_archive_settings);

    let read_strings = reader
        .read_strings((archive_settings.num_user_files + archive_settings.num_directories) as usize)
        .unwrap();
    assert_eq!(strings, read_strings);

    let read_map = reader
        .read_file_chunk_map(archive_settings.num_user_files as usize)
        .unwrap();
    assert_eq!(map, read_map);

    let read_chunk_settings = reader.read_chunk_settings().unwrap();
    assert_eq!(chunk_settings, read_chunk_settings);

    let read_chunks = reader
        .read_chunks(chunk_settings.num_chunks as usize)
        .unwrap();
    assert_eq!(chunks, read_chunks);

    // Spec: File List (ChunkSettings.NumArchiveFiles -1 list of null-terminated files)
    let read_file_list = reader
        .read_file_list((chunk_settings.num_archive_files - 1) as usize)
        .unwrap();
    assert_eq!(file_list, read_file_list);

    let read_global_settings = reader.read_global_settings().unwrap();
    assert_eq!(global_settings, read_global_settings);
}
