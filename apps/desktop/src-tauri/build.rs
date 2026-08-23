use std::fs;
use std::path::{Path, PathBuf};

const FRONTEND_DIST: &str = "../dist/renderer";

fn main() {
    // Tauri CLI sets this deprecated variable and overrides tauri.conf.json.
    // SAFETY: Cargo runs this build script single-threaded before other build work.
    unsafe { std::env::remove_var("STATIC_VCRUNTIME") };
    tauri_build::build();

    let out_dir = std::env::var_os("OUT_DIR").map(PathBuf::from);
    let Some(out_dir) = out_dir else {
        return;
    };
    let stamp_path = out_dir.join("slopcast-frontend-stamp");

    let dist = Path::new(FRONTEND_DIST);
    if !dist.is_dir() {
        let _ = fs::write(&stamp_path, "no-frontend");
        return;
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dist, &mut files);
    files.sort();

    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    println!("cargo:rerun-if-changed={}", dist.display());

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
