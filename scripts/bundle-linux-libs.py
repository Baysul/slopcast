"""Copy linked native/plugin libraries for the AppDir; leave libc and graphics drivers to the host."""
import pathlib
import re
import shutil
import subprocess
import sys

app = pathlib.Path(sys.argv[1]).resolve()
destination = app / "lib"
destination.mkdir(exist_ok=True)
pending = [app / "slopcast", *app.glob("resources/gstreamer-plugins/*.so"), app / "resources/gstreamer-plugins/gst-plugin-scanner"]
seen = set()
# Bundling a different libc or host-specific GPU driver breaks the user's graphics stack.
host = re.compile(r"^(ld-linux|lib(c|m|pthread|dl|rt|resolv|nss_[^.]+)\.so|lib(vulkan_[^.]+|GLX_[^.]+|nvidia[^.]*|drm[^.]*)\.so)")
while pending:
    binary = pending.pop()
    if binary in seen:
        continue
    seen.add(binary)
    output = subprocess.run(["ldd", str(binary)], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout
    for line in output.splitlines():
        if "not found" in line:
            raise SystemExit(f"Unresolved library in {binary}: {line.strip()}")
        match = re.search(r"\s*(\S+)\s+=>\s+(/\S+)", line)
        if not match:
            continue
        name, source = match.groups()
        if host.match(name):
            continue
        target = destination / name
        if not target.exists():
            shutil.copy2(source, target)
            pending.append(target)
