use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Enable verbose logging (Debug level) for troubleshooting.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Unpack a .dz archive
    Unpack {
        /// Input .dz file
        input: PathBuf,

        /// Optional output directory (default: same as input filename stem)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Keep raw data if decompression fails
        #[arg(short, long)]
        keep_raw: bool,
    },
    /// Pack a directory into a .dz archive based on a .toml config
    Pack {
        /// Input .toml configuration file
        config: PathBuf,
    },
    /// List contents of a .dz archive without unpacking
    List {
        /// Input .dz file
        input: PathBuf,
    },
}
