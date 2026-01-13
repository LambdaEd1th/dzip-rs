# dzip-core

**dzip-core** is the underlying library powering `dzip-cli`, designed for high-performance unpacking and packing of **Marmalade SDK** resource archives (`.dz` / `.dzip`).

It exposes a robust, thread-safe API for handling legacy archive formats, supporting multi-volume split archives, directory reconstruction, and various compression algorithms.

## ✨ Features

* **🚀 High Performance**: Built on top of `rayon` for parallel compression and extraction.
* **🛡️ Safe & Robust**: Handles path sanitization to prevent directory traversal attacks and fixes common legacy header errors (e.g., incorrect ZSIZE).
* **🧩 Modular Design**: Decoupled UI/Logging via the `DzipObserver` trait.
* **🗜️ Comprehensive Compression Support**:
* LZMA (Legacy)
* ZLIB (Deflate)
* BZIP2
* Store (Copy)
* Zero-block generation



## 📦 Installation

Add `dzip-core` to your `Cargo.toml`.

```toml
[dependencies]
dzip-core = { path = "crates/core" } # Adjust path if necessary
# Or if published:
# dzip-core = "0.2.0"

```

## 📖 Usage

### 1. Basic Setup

Most operations require a `CodecRegistry` (to handle compression algorithms) and a `DzipObserver` (to handle progress feedback).

```rust
use dzip_core::{create_default_registry, DzipObserver, NoOpObserver};

// 1. Create the default codec registry (includes LZMA, ZLIB, BZIP2, etc.)
let registry = create_default_registry();

// 2. Use NoOpObserver if you don't need progress feedback,
//    or implement your own DzipObserver for custom UI integration.
let observer = NoOpObserver;

```

### 2. Unpacking an Archive

Use `do_unpack` to extract a `.dz` file. This function also generates a `.toml` configuration file needed for repacking.

```rust
use std::path::PathBuf;
use dzip_core::do_unpack;

fn main() -> anyhow::Result<()> {
    let input = PathBuf::from("assets/data.dz");
    let output_dir = Some(PathBuf::from("extracted_assets"));
    let keep_raw = false; // Set to true to dump raw data if decompression fails
    
    let registry = dzip_core::create_default_registry();
    let observer = dzip_core::NoOpObserver;

    // Perform the unpack operation
    do_unpack(&input, output_dir, keep_raw, &registry, &observer)?;
    
    println!("Unpack successful!");
    Ok(())
}

```

### 3. Packing an Archive

Use `do_pack` to create a `.dz` archive based on a TOML configuration file. The packer will look for the source files in the directory relative to the config file.

```rust
use std::path::PathBuf;
use dzip_core::do_pack;

fn main() -> anyhow::Result<()> {
    // Point to the TOML config generated during unpack (or manually created)
    let config_path = PathBuf::from("extracted_assets/data.toml");
    
    let registry = dzip_core::create_default_registry();
    let observer = dzip_core::NoOpObserver;

    // Perform the pack operation
    do_pack(&config_path, &registry, &observer)?;
    
    println!("Pack successful!");
    Ok(())
}

```

### 4. Custom Progress Reporting

Implement the `DzipObserver` trait to integrate with your application's UI (e.g., CLI progress bars, GUI status bars, or logs).

```rust
use dzip_core::DzipObserver;

struct MyLogger;

impl DzipObserver for MyLogger {
    fn info(&self, message: &str) {
        println!("[INFO] {}", message);
    }

    fn warn(&self, message: &str) {
        eprintln!("[WARN] {}", message);
    }

    fn progress_start(&self, total_items: u64) {
        println!("Starting processing of {} items...", total_items);
    }

    fn progress_inc(&self, delta: u64) {
        // Handle incremental updates (e.g., update a progress bar)
    }

    fn progress_finish(&self, message: &str) {
        println!("Finished: {}", message);
    }
}

```

## ⚙️ Architecture

* **CodecRegistry**: A plugin-like system where you can register `Compressor` and `Decompressor` implementations.
* **Parallel Pipeline**: Unpacking uses parallel iterators to extract files, while packing uses a producer-consumer model (parallel compression + sequential writing) to maximize throughput while ensuring file integrity.

## 📄 License

This project is licensed under the **GNU General Public License v3.0**. See the `LICENSE` file for details.