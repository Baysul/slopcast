"""Invoke NSIS with native paths/options, including from a Git Bash build."""
import os
import pathlib
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
binary, version, output = sys.argv[1:]
option = "/D" if os.name == "nt" else "-D"
defines = {
    "VERSION": version,
    "BINARY": str(pathlib.Path(binary).resolve()),
    "OUTPUT": str(pathlib.Path(output).resolve()),
    "ICON": str(root / "apps/desktop/resources/icons/icon.ico"),
    "LICENSES": str(root / "apps/desktop/resources/fonts"),
}
subprocess.run(["makensis", *[f"{option}{key}={value}" for key, value in defines.items()], str(root / "apps/desktop/resources/installer.nsi")], check=True)
