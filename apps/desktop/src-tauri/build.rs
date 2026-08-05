//! Build script for the Slopcast Tauri desktop app.
//!
//! In addition to the standard `tauri_build::build()`, this tracks every file
//! under the renderer `dist/` and forces `slopcast_lib` to recompile — and
//! therefore re-run `generate_context!`, which embeds the frontend at compile
//! time — whenever the bundle changes. Cargo's own `include_bytes!` tracking
//! only covers assets from the *previous* build, so a Vite bundle that emits
//! new content-hashed filenames would otherwise leave the app embedding a
//! stale frontend (a blank window). The stamp file the crate includes below
//! makes any dist change propagate to a fresh embed.

use std::fs;
use std::path::{Path, PathBuf};

/// Path to the built renderer bundle, relative to this crate directory
/// (mirrors `frontendDist` in `tauri.conf.json`).
const FRONTEND_DIST: &str = "../dist/renderer";

fn main() {
    tauri_build::build();

    let dist = Path::new(FRONTEND_DIST);
    if !dist.is_dir() {
        // tauri_codegen already panics with a precise message when the
        // frontend bundle is required for an embedded build.
        return;
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dist, &mut files);
    files.sort();

    // Re-run this script whenever any asset changes, including assets that
    // appear after the first build (new hashed filenames).
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    println!("cargo:rerun-if-changed={}", dist.display());

    // Persist a fingerprint of every asset; the crate includes this file, so a
    // content change here forces `slopcast_lib` to recompile and re-embed.
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let stamp_path = out_dir.join("slopcast-frontend-stamp");
    let stamp = format!("{:016x}", hash_files(&files));
    if fs::read_to_string(&stamp_path).ok().as_deref() != Some(stamp.as_str()) {
        let _ = fs::write(&stamp_path, stamp);
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Stable FNV-1a over every asset's path and content (std hashers are
/// randomized per process, so they cannot be persisted across builds).
fn hash_files(files: &[PathBuf]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for file in files {
        for &byte in file.to_string_lossy().as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        if let Ok(bytes) = fs::read(file) {
            for &byte in &bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}
