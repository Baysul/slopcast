use std::process::{exit, Command};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-targets") => check_targets(),
        Some(cmd) => {
            eprintln!("unknown xtask subcommand: {cmd}");
            exit(1);
        }
        None => {
            eprintln!("usage: cargo xtask <subcommand>");
            exit(1);
        }
    }
}

fn check_targets() {
    let cross_targets: &[&str] = match std::env::consts::OS {
        "linux" => &["x86_64-pc-windows-msvc", "aarch64-apple-darwin"],
        "macos" => &["x86_64-pc-windows-msvc"],
        _ => &[],
    };

    if !cross_targets.is_empty() {
        let status = Command::new("rustup")
            .args(["target", "add"])
            .args(cross_targets)
            .status()
            .expect("failed to run rustup");
        if !status.success() {
            exit(status.code().unwrap_or(1));
        }
    }

    for &target in cross_targets {
        let mut cmd = Command::new("cargo");
        cmd.arg("check").arg("--target").arg(target);
        // ffmpeg-sys-next links against system FFmpeg libraries that only
        // exist for the host platform, so cross-target checks must run
        // without the ffmpeg feature (the stubs take over).
        cmd.arg("--no-default-features");
        if target.contains("apple") {
            cmd.env("DOCS_RS", "1");
        }
        let status = cmd.status().expect("failed to run cargo check");
        if !status.success() {
            exit(status.code().unwrap_or(1));
        }
    }
}
