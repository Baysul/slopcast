"""Reject embedded browser shells in each supported native desktop dependency graph."""
import re
import subprocess

forbidden = re.compile(r"^(?:tauri(?:-[\w-]+)?|cef(?:-[\w-]+)?|wry|webkit2gtk(?:-sys)?|webview2-com(?:-sys)?) v", re.MULTILINE)
# --target all would incorrectly combine Windows-only dependencies of Linux-only crates.
for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]:
    resolved = subprocess.run(
        ["cargo", "tree", "--locked", "-p", "slopcast", "--prefix", "none", "--edges", "normal,build", "--target", target],
        text=True, stdout=subprocess.PIPE, check=True,
    ).stdout
    matches = forbidden.findall(resolved)
    if matches:
        raise SystemExit(f"Embedded browser dependency for {target}: {matches}")
    print(f"{target}: native desktop dependency graph contains no embedded browser shell.")
