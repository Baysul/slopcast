"""Exercise the real native window, services and synthetic preview under Xvfb.

Build first: cargo build -p slopcast --features e2e
Requires Xvfb, xdotool, scrot, tesseract and ImageMagick. No browser, portal or LiveKit server.
"""
import csv
import ctypes
import io
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "test-output/native-smoke"
OUTPUT.mkdir(parents=True, exist_ok=True)


class API(BaseHTTPRequestHandler):
    requests = []

    def log_message(self, *_args):
        pass

    def reply(self):
        API.requests.append({"method": self.command, "path": self.path, "origin": self.headers.get("X-Client-Origin")})
        status, payload = (200, {"status": "ok"}) if self.path == "/api/health" else (503, {"error": "Native smoke test rejection"})
        raw = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    do_GET = do_POST = reply


def wait_for(description, predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {description}")


with tempfile.TemporaryDirectory(prefix="slopcast-native-smoke-") as temporary:
    temporary = pathlib.Path(temporary)
    api = ThreadingHTTPServer(("127.0.0.1", 0), API)
    threading.Thread(target=api.serve_forever, daemon=True).start()
    server_log = (OUTPUT / "xvfb.log").open("w")
    xvfb = subprocess.Popen(["Xvfb", "-displayfd", "1", "-noreset", "-screen", "0", "1600x1000x24", "-nolisten", "tcp"], stdout=subprocess.PIPE, stderr=server_log, text=True)
    app = None
    samples = []
    try:
        number = xvfb.stdout.readline().strip()
        assert number.isdigit(), "Xvfb did not start"
        env = os.environ.copy()
        env.pop("WAYLAND_DISPLAY", None)
        env.update(DISPLAY=":" + number, XDG_SESSION_TYPE="x11", SLOPCAST_SETTINGS_DIR=str(temporary / "settings"),
                   XDG_DATA_HOME=str(temporary / "data"), DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(temporary / "no-bus"),
                   SLOPCAST_E2E_CAPTURE="synthetic", SLOPCAST_E2E_PREVIEW_ONLY="1",
                   RUST_LOG="info,slopcast::services=debug", SLOPCAST_E2E_METRICS=str(temporary / "metrics.json"), API_ENDPOINT=f"http://127.0.0.1:{api.server_address[1]}", OMP_THREAD_LIMIT="1")
        icds = pathlib.Path("/usr/share/vulkan/icd.d")
        lavapipe = next(icds.glob("lvp*.json"), None)
        if lavapipe:
            env["VK_ICD_FILENAMES"] = str(lavapipe)
        settings = temporary / "settings"
        settings.mkdir()
        (settings / "onboarding.json").write_text('{"completed":true}')
        binary = pathlib.Path(os.environ.get("SLOPCAST_SMOKE_BINARY", ROOT / "target/debug/slopcast"))
        app_log = (OUTPUT / "app.log").open("w")
        app = subprocess.Popen([str(binary)], cwd=ROOT, env=env, stdout=app_log, stderr=subprocess.STDOUT)

        def run(args):
            return subprocess.run(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True, timeout=15).stdout.strip()

        def windows(title):
            result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", title], env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
            return result.stdout.strip().splitlines() if result.returncode == 0 else []

        def global_key():
            # Send physical keycodes without a keysym tool rewriting held modifiers.
            x11 = ctypes.CDLL("libX11.so.6")
            xtest = ctypes.CDLL("libXtst.so.6")
            x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
            x11.XOpenDisplay.restype = ctypes.c_void_p
            x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
            x11.XStringToKeysym.restype = ctypes.c_ulong
            x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
            x11.XKeysymToKeycode.restype = ctypes.c_ubyte
            x11.XFlush.argtypes = [ctypes.c_void_p]
            x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
            xtest.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
            display = x11.XOpenDisplay(env["DISPLAY"].encode())
            assert display, "Cannot connect XTEST keyboard"
            try:
                codes = [x11.XKeysymToKeycode(display, x11.XStringToKeysym(key)) for key in [b"Control_L", b"Shift_L", b"F11"]]
                for code in codes:
                    assert xtest.XTestFakeKeyEvent(display, code, True, 0)
                x11.XFlush(display)
                time.sleep(.15)
                for code in reversed(codes):
                    assert xtest.XTestFakeKeyEvent(display, code, False, 0)
                x11.XFlush(display)
            finally:
                x11.XCloseDisplay(display)

        def metrics():
            try:
                return json.loads((temporary / "metrics.json").read_text())
            except (OSError, json.JSONDecodeError):
                return {}

        def screenshot(window, name):
            geometry = dict(line.split("=", 1) for line in run(["xdotool", "getwindowgeometry", "--shell", window]).splitlines())
            path = OUTPUT / f"{name}.png"
            rectangle = ",".join(geometry[key] for key in ("X", "Y", "WIDTH", "HEIGHT"))
            run(["scrot", "-a", rectangle, "-o", str(path)])
            return path

        def locate(window, phrase):
            image = screenshot(window, "locator")
            scale, offset = 1, 0
            magick = shutil.which("magick") or shutil.which("convert")
            if magick and phrase in {"Create room only", "Mute audio source", "Unmute audio source", "Cancel", "Go live"}:
                geometry = dict(line.split("=", 1) for line in run(["xdotool", "getwindowgeometry", "--shell", window]).splitlines())
                offset = int(geometry["WIDTH"]) - 352
                region = OUTPUT / "controls-locator.png"
                run([magick, str(image), "-crop", f"352x{geometry['HEIGHT']}+{offset}+0", "-channel", "R", "-separate", "-resize", "300%", str(region)])
                image_for_text, scale = region, 3
            else:
                image_for_text = image
            tsv = run(["tesseract", str(image_for_text), "stdout", "--psm", "11", "tsv"])
            rows = list(csv.DictReader(io.StringIO(tsv), delimiter="\t"))
            lines = {}
            for word in rows:
                if word["text"].strip():
                    key = (word["block_num"], word["par_num"], word["line_num"])
                    lines.setdefault(key, []).append(word)
            wanted = phrase.casefold().split()
            for line in lines.values():
                for index in range(len(line) - len(wanted) + 1):
                    words = line[index:index + len(wanted)]
                    if [word["text"].casefold() for word in words] == wanted:
                        left = min(int(word["left"]) for word in words)
                        top = min(int(word["top"]) for word in words)
                        right = max(int(word["left"]) + int(word["width"]) for word in words)
                        bottom = max(int(word["top"]) + int(word["height"]) for word in words)
                        y = (top + bottom) // (2 * scale)
                        if phrase == "Slopcast" and y < 150:
                            continue
                        return str((left + right) // (2 * scale) + offset), str(y)
            # Sparse-page OCR can ignore dark text inside a light primary button.
            # Find light button-shaped regions, then OCR each in its own color context.
            magick = shutil.which("magick") or shutil.which("convert")
            if magick:
                components = run([magick, str(image), "-colorspace", "gray", "-threshold", "50%", "-define", "connected-components:verbose=true", "-connected-components", "8", "null:"])
                for line in components.splitlines():
                    match = re.search(r"(\d+)x(\d+)\+(\d+)\+(\d+).*gray\(255\)", line)
                    if not match:
                        continue
                    width, height, left, top = map(int, match.groups())
                    if width < 40 or not 15 <= height <= 80 or width < height * 1.2:
                        continue
                    region = OUTPUT / "button-locator.png"
                    run([magick, str(image), "-crop", f"{width}x{height}+{left}+{top}", "-resize", "300%", str(region)])
                    text = run(["tesseract", str(region), "stdout", "--psm", "7"])
                    if re.sub(r"[^\w\s]", " ", text.casefold()).split() == phrase.casefold().split():
                        return str(left + width // 2), str(top + height // 2)
            raise AssertionError(f"No visible control '{phrase}' in {image}")

        def click(window, phrase):
            x, y = locate(window, phrase)
            run(["xdotool", "mousemove", "--window", window, x, y, "click", "1"])
            time.sleep(0.3)

        def close(window):
            geometry = dict(line.split("=", 1) for line in run(["xdotool", "getwindowgeometry", "--shell", window]).splitlines())
            # Xvfb has no compositor: GPUI uses server decorations without a client shadow.
            # The custom GNOME close control sits 24px from the right edge.
            run(["xdotool", "mousemove", "--window", window, str(int(geometry["WIDTH"]) - 24), "20", "click", "1"])

        main = wait_for("main window", lambda: windows("^Slopcast$") and windows("^Slopcast$")[0])
        wait_for("native frames painted", lambda: metrics().get("frames", 0) > 5 and metrics().get("paints", 0) > 2)
        assert metrics()["colorsCorrect"], "BGRA preview channels are wrong"
        create_position = locate(main, "Create room only")
        mute_position = locate(main, "Mute audio source")
        click(main, "Mute audio source")
        wait_for("mute command", lambda: metrics().get("muted"))
        run(["xdotool", "windowfocus", main])
        global_key()
        wait_for("native global shortcut", lambda: metrics().get("muted") is False)
        second = subprocess.run([str(binary)], cwd=ROOT, env=env, timeout=10, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert second.returncode == 0, "Second launch did not activate the first instance"
        assert len(windows("^Slopcast$")) == 1, "Second launch created another presenter"
        run(["xdotool", "windowfocus", main, "key", "ctrl+comma"])
        settings_window = wait_for("native settings", lambda: windows("^Slopcast Settings$") and windows("^Slopcast Settings$")[0])
        run(["xdotool", "windowraise", settings_window, "windowfocus", settings_window])
        time.sleep(0.8)
        screenshot(settings_window, "02-settings")
        click(settings_window, "Appearance")
        screenshot(settings_window, "02-appearance")
        click(settings_window, "Light")
        click(settings_window, "Ayu")
        wait_for("appearance preferences", lambda: metrics().get("preferences", {}).get("colorTheme") == "ayu")
        screenshot(settings_window, "02-appearance-light-ayu")
        click(settings_window, "Slopcast")
        click(settings_window, "Dark")
        wait_for("original theme restored", lambda: metrics().get("preferences", {}).get("colorTheme") == "slopcast")
        click(settings_window, "Stream")
        screenshot(settings_window, "02-stream")
        click(settings_window, "Shortcuts")
        screenshot(settings_window, "02-shortcuts")
        run(["xdotool", "windowfocus", settings_window, "key", "Escape"])
        wait_for("settings closes", lambda: not windows("^Slopcast Settings$"))
        run(["xdotool", "windowfocus", main])
        click(main, "Create room only")
        wait_for("desktop API request", lambda: any(r["method"] == "POST" and r["origin"] == "desktop" for r in API.requests))
        screenshot(main, "03-preview")
        time.sleep(1)
        for _ in range(7):
            status = pathlib.Path(f"/proc/{app.pid}/status").read_text().splitlines()
            rss = int(next(line for line in status if line.startswith("VmRSS:")).split()[1])
            samples.append({"rssKiB": rss, "frames": metrics().get("frames")})
            time.sleep(2)
        assert samples[-1]["frames"] > samples[0]["frames"] + 20, "Preview stalled"
        assert samples[-1]["rssKiB"] - samples[0]["rssKiB"] < 128 * 1024, "Preview textures accumulated instead of being evicted"
        run(["xdotool", "windowsize", main, "900", "650"])
        wait_for("physical preview viewport resize", lambda: metrics().get("viewport") is not None and 400 <= metrics()["viewport"][0] < 500 and abs(metrics()["viewport"][0] / metrics()["viewport"][1] - 16/9) < .02)
        screenshot(main, "04-resized")
        run(["xdotool", "windowunmap", main])
        wait_for("hidden preview disabled", lambda: metrics().get("viewport") is None)
        time.sleep(0.5)
        hidden = metrics()["frames"]
        time.sleep(2)
        assert metrics()["frames"] <= hidden + 1, "Hidden window kept converting previews"
        run(["xdotool", "windowmap", main, "windowsize", main, "1100", "900"])
        wait_for("preview resumes", lambda: metrics().get("frames", 0) > hidden + 5)
        click(main, "Cancel")
        wait_for("capture stopped", lambda: metrics().get("captureActive") is False)
        time.sleep(0.8)
        assert metrics()["viewport"] is None, "Idle render re-registered the cleared viewport"
        screenshot(main, "05-stopped")
        close(main)
        app.wait(timeout=10)
        assert app.returncode == 0, f"Desktop exited {app.returncode}"
        # Repeat with the same real controls behind a confirmation. Coordinates were
        # measured in the unobscured window above, so dimming does not affect OCR.
        env["SLOPCAST_E2E_DIALOG"] = "1"
        (temporary / "metrics.json").unlink(missing_ok=True)
        app = subprocess.Popen([str(binary)], cwd=ROOT, env=env, stdout=app_log, stderr=subprocess.STDOUT)
        main = wait_for("confirmation window", lambda: windows("^Slopcast$") and windows("^Slopcast$")[0])
        wait_for("confirmation fixture", lambda: metrics().get("dialogOpen") and metrics().get("frames", 0)>5)
        before = len(API.requests)
        for x, y in [create_position, mute_position]:
            run(["xdotool", "mousemove", "--window", main, x, y, "click", "1"])
        time.sleep(.5)
        assert len(API.requests) == before, "Confirmation allowed a background room request"
        assert not metrics()["muted"], "Confirmation allowed a background mute click"
        screenshot(main, "01-modal")
        run(["xdotool", "windowfocus", main])
        run(["xdotool", "key", "Escape"])
        wait_for("Escape dismissed confirmation", lambda: not metrics().get("dialogOpen", True))
        close(main)
        app.wait(timeout=10)
        assert app.returncode == 0, f"Confirmation test exited {app.returncode}"
        (OUTPUT / "result.json").write_text(json.dumps({"ok": True, "memorySamples": samples, "requests": API.requests}, indent=2))
        print(f"Native Xvfb checks passed. Artifacts: {OUTPUT}")
    finally:
        if app is not None and app.poll() is None:
            app.terminate()
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait()
        xvfb.terminate()
        xvfb.wait(timeout=5)
        api.shutdown()
        api.server_close()
        if (temporary / "metrics.json").exists():
            (OUTPUT / "last-metrics.json").write_text((temporary / "metrics.json").read_text())
        (OUTPUT / "memory.json").write_text(json.dumps(samples, indent=2))
