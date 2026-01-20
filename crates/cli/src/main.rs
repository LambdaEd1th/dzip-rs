mod args;
mod fs;

use clap::Parser;
use log::{LevelFilter, info};
use std::fs as std_fs;
use std::path::Path;

use args::{Cli, Commands};
use fs::{FsPackSink, FsPackSource, FsUnpackSink, FsUnpackSource};
use indicatif::{ProgressBar, ProgressStyle};

use dzip_core::utils::to_native_path;
use dzip_core::{Result, do_list, do_pack, do_unpack, model::Config};

fn main() {
    let cli = Cli::parse();
    let mut builder = env_logger::Builder::from_default_env();
    if std::env::var("RUST_LOG").is_err() {
        builder.filter(None, LevelFilter::Info);
    }
    if cli.verbose {
        builder.filter(None, LevelFilter::Debug);
    }
    builder.init();

    let run = || -> Result<()> {
        let pb = ProgressBar::new(0);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
        );

        let on_progress = move |event: dzip_core::ProgressEvent| match event {
            dzip_core::ProgressEvent::Start(total) => {
                pb.set_length(total as u64);
                pb.enable_steady_tick(std::time::Duration::from_millis(100));
            }
            dzip_core::ProgressEvent::Inc(n) => pb.inc(n as u64),
            dzip_core::ProgressEvent::Finish => pb.finish_and_clear(),
        };

        match &cli.command {
            Commands::Unpack {
                input,
                output,
                keep_raw,
            } => {
                let base_dir = input.parent().unwrap_or(Path::new(".")).to_path_buf();
                let file_name = input
                    .file_name()
                    .ok_or_else(|| dzip_core::DzipError::Generic("Invalid input filename".into()))?
                    .to_string_lossy()
                    .to_string();
                let source = FsUnpackSource {
                    base_path: base_dir,
                    main_file_name: file_name,
                };
                let base_stem = input
                    .file_stem()
                    .ok_or_else(|| {
                        dzip_core::DzipError::Generic("Input path has no file stem".into())
                    })?
                    .to_string_lossy();
                let out_dir = output
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from(base_stem.to_string()));
                std_fs::create_dir_all(&out_dir).map_err(|e| {
                    dzip_core::DzipError::IoContext(out_dir.display().to_string(), e)
                })?;

                let sink = FsUnpackSink {
                    output_dir: out_dir,
                };

                // Config now contains OS-native paths (e.g., backslashes on Windows)
                let config = do_unpack(&source, &sink, *keep_raw, on_progress)?;

                let toml_str =
                    toml::to_string_pretty(&config).map_err(dzip_core::DzipError::TomlSer)?;
                let config_path = format!("{}.toml", base_stem);
                std_fs::write(&config_path, toml_str)
                    .map_err(|e| dzip_core::DzipError::IoContext(config_path.clone(), e))?;
                info!("Config saved to {}", config_path);
                Ok(())
            }
            Commands::Pack { config } => {
                let toml_content = std_fs::read_to_string(config).map_err(|e| {
                    dzip_core::DzipError::IoContext(config.display().to_string(), e)
                })?;
                let mut core_config: Config =
                    toml::from_str(&toml_content).map_err(dzip_core::DzipError::TomlDe)?;

                for file in &mut core_config.files {
                    // Normalize inputs to OS-native format to ensure they can be found on disk.
                    // This handles splitting Unix-style paths on Windows, or Windows-style on Unix.
                    file.path = to_native_path(Path::new(&file.path));
                }

                let config_parent = config.parent().unwrap_or(Path::new(".")).to_path_buf();
                let base_name = config
                    .file_stem()
                    .ok_or_else(|| {
                        dzip_core::DzipError::Generic("Config file path has no stem".into())
                    })?
                    .to_string_lossy()
                    .to_string();
                let source = FsPackSource {
                    root_dir: config_parent.join(&base_name),
                };
                let mut sink = FsPackSink {
                    output_dir: config_parent,
                    base_name: base_name.clone(),
                };
                do_pack(core_config, base_name, &mut sink, &source, on_progress)
            }
            Commands::List { input } => {
                let base_dir = input.parent().unwrap_or(Path::new(".")).to_path_buf();
                let file_name = input
                    .file_name()
                    .ok_or_else(|| dzip_core::DzipError::Generic("Invalid input filename".into()))?
                    .to_string_lossy()
                    .to_string();
                let source = FsUnpackSource {
                    base_path: base_dir,
                    main_file_name: file_name,
                };
                let entries = do_list(&source)?;

                println!();
                println!("{:<15} | {:<8} | Path", "Size (Bytes)", "Chunks");
                println!("{:-<15}-|-{:-<8}-|--------------------------------", "", "");
                for entry in &entries {
                    // Use native path separators for display
                    let display_path = to_native_path(Path::new(&entry.path));
                    println!(
                        "{:<15} | {:<8} | {}",
                        entry.original_size, entry.chunk_count, display_path
                    );
                }
                println!("\nTotal files: {}", entries.len());
                Ok(())
            }
        }
    };

    if let Err(e) = run() {
        eprintln!("\x1b[31mError:\x1b[0m {:#}", e);
        std::process::exit(1);
    }
}
