#!/usr/bin/env python3
"""Capture the native inspection UI at 100 sizes and write a review gallery."""

import argparse
import errno
import html
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
WIDTHS = (320, 375, 480, 640, 768, 800, 1024, 1280, 1440, 1920)
HEIGHTS = (240, 320, 360, 480, 600, 640, 720, 800, 900, 1080)
PNG_END = b"\x00\x00\x00\x00IEND\xaeB`\x82"


def sizes_argument(value):
    try:
        sizes = [tuple(map(int, item.lower().split("x"))) for item in value.split(",")]
        if any(len(size) != 2 or min(size) < 128 or max(size) > 4096 for size in sizes):
            raise ValueError
        if len(set(sizes)) != len(sizes):
            raise ValueError
        return sizes
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "Use unique WIDTHxHEIGHT pairs, each dimension between 128 and 4096."
        ) from error


def command(args):
    return subprocess.check_output(args, cwd=ROOT, text=True, stderr=subprocess.STDOUT)


def find_binary(override):
    if override:
        binary = Path(override).resolve()
    else:
        build = ["cargo", "build", "-p", "chatty-gui", "--locked", "--offline"]
        if shutil.which("rtk"):
            build.insert(0, "rtk")
        subprocess.run(build, cwd=ROOT, check=True)
        metadata = json.loads(command([
            "cargo", "metadata", "--no-deps", "--format-version", "1", "--offline", "--locked"
        ]))
        binary = Path(metadata["target_directory"]) / "debug" / "chatty-gui"
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError(f"GUI binary is not executable: {binary}")
    return binary


class Inspector:
    def __init__(self, process, fifo, timeout, settle):
        self.process = process
        self.fifo = fifo
        self.timeout = timeout
        self.settle = settle
        self.hypr_window = None
        self.hypr_lua = False

    def send(self, message):
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("Inspection app exited; see app.log.")
            try:
                descriptor = os.open(self.fifo, os.O_WRONLY | os.O_NONBLOCK)
            except OSError as error:
                if error.errno not in (errno.ENXIO, errno.ENOENT):
                    raise
                time.sleep(0.05)
                continue
            try:
                os.write(descriptor, (message + "\n").encode())
            finally:
                os.close(descriptor)
            # The existing FIFO reader opens a fresh reader for each command.
            time.sleep(self.settle)
            return
        raise RuntimeError("Timed out waiting for the inspection FIFO; see app.log.")

    def prepare(self):
        if not os.environ.get("HYPRLAND_INSTANCE_SIGNATURE") or not shutil.which("hyprctl"):
            return
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            clients = json.loads(command(["hyprctl", "-j", "clients"]))
            window = next((item for item in clients if item.get("pid") == self.process.pid), None)
            if window:
                self.hypr_window = window["address"]
                result = subprocess.run(
                    ["hyprctl", "repl", "type(hl.dsp.window.resize)"],
                    capture_output=True, text=True, check=False,
                )
                self.hypr_lua = result.returncode == 0 and "function" in result.stdout
                if not window.get("floating"):
                    if self.hypr_lua:
                        expression = f'hl.dsp.window.float({{window="address:{self.hypr_window}"}})'
                        command(["hyprctl", "dispatch", expression])
                    else:
                        command(["hyprctl", "dispatch", "togglefloating", f"address:{self.hypr_window}"])
                    time.sleep(self.settle)
                return
            time.sleep(0.05)
        raise RuntimeError("Could not find this inspection app's Hyprland window.")

    def resize(self, width, height):
        if self.hypr_window:
            if self.hypr_lua:
                expression = (
                    f'hl.dsp.window.resize({{x={width},y={height},relative=false,'
                    f'window="address:{self.hypr_window}"}})'
                )
                command(["hyprctl", "dispatch", expression])
            else:
                command(["hyprctl", "dispatch", "resizewindowpixel",
                         f"exact {width} {height},address:{self.hypr_window}"])
            time.sleep(self.settle)
        else:
            self.send(f"resize {width} {height}")

    def capture(self, path):
        path.unlink(missing_ok=True)
        self.send(f"screenshot {path}")
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("Inspection app exited while capturing; see app.log.")
            if path.exists():
                data = path.read_bytes()
                if data.startswith(b"\x89PNG\r\n\x1a\n") and data.endswith(PNG_END):
                    return data, struct.unpack(">II", data[16:24])
            time.sleep(0.05)
        raise RuntimeError("Timed out waiting for a complete PNG screenshot; see app.log.")


def write_report(output, results, requested, theme):
    verified = sum(item["status"] == "captured" for item in results)
    report = {"theme": theme, "requested": len(requested), "captured": verified, "results": results}
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    cards = []
    for result in results:
        title = f'{result["width"]} × {result["height"]}'
        if result.get("image"):
            image = html.escape(result["image"], quote=True)
            picture = f'<a href="{image}"><img loading="lazy" src="{image}" alt="Chatty at {title}"></a>'
        else:
            picture = ""
        detail = html.escape(result.get("error", "Dimensions verified · review layout visually"))
        cards.append(f'<article><h2>{title}</h2>{picture}<p>{detail}</p></article>')
    page = f'''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Chatty responsive inspection</title>
<style>
:root{{color-scheme:{theme};font:15px var(--font-sans,system-ui)}}body{{margin:0}}
main{{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(250px,100%),1fr));gap:16px}}
article{{border:1px solid var(--border,GrayText);border-radius:var(--radius,12px);padding:16px;min-width:0}}
h2{{font-size:16px;margin:0 0 12px}}img{{display:block;width:100%;height:240px;object-fit:contain}}
p{{font-size:13px;line-height:1.5}}a:focus-visible{{outline:3px solid var(--ring,Highlight)}}
</style>
<p>{verified} / {len(requested)} sizes captured · {theme} theme. Click an image for full size.</p>
<p>Capture checks verify PNG dimensions. Review the images for clipping, overlap, and usability.</p>
<main>{"".join(cards)}</main></html>'''
    (output / "index.html").write_text(page)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", help="Use an existing GUI binary instead of building offline.")
    parser.add_argument("--output", type=Path, default=Path("/tmp") / time.strftime("chatty-responsive-%Y%m%d-%H%M%S"))
    parser.add_argument("--sizes", type=sizes_argument, default=[(w, h) for w in WIDTHS for h in HEIGHTS])
    parser.add_argument("--theme", choices=("dark", "light"), default="dark")
    parser.add_argument("--timeout", type=float, default=10.0, help="Seconds allowed for each control/capture operation.")
    parser.add_argument("--settle", type=float, default=0.25, help="Seconds to settle after a control command.")
    args = parser.parse_args()
    if args.timeout <= 0 or args.settle <= 0:
        parser.error("--timeout and --settle must be positive")
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error(f"Output directory must be empty: {output}")
    output.mkdir(parents=True, exist_ok=True)
    results = []
    try:
        binary = find_binary(args.binary)
        with tempfile.TemporaryDirectory(prefix="chatty-responsive-") as temporary:
            temporary = Path(temporary)
            fifo = temporary / "control"
            os.mkfifo(fifo, 0o600)
            with (output / "app.log").open("w") as log:
                process = subprocess.Popen([
                    str(binary), "--inspect", "--inspect-control", str(fifo),
                    "--width", "800", "--height", "600",
                    "--session-file", str(temporary / "session"),
                ], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
                inspector = Inspector(process, fifo, args.timeout, args.settle)
                try:
                    inspector.send(f"theme {args.theme}")
                    inspector.prepare()
                    for index, (width, height) in enumerate(args.sizes, 1):
                        entry = {"width": width, "height": height, "status": "failed"}
                        results.append(entry)
                        for _ in range(3):
                            inspector.resize(width, height)
                            data, actual = inspector.capture(temporary / "capture.png")
                            if actual == (width, height):
                                break
                        name = f"{width}x{height}.png"
                        (output / name).write_bytes(data)
                        entry.update(image=name, actual_width=actual[0], actual_height=actual[1])
                        if actual != (width, height):
                            entry["error"] = f"Requested {width}×{height}, captured {actual[0]}×{actual[1]}. Check compositor resizing and display scale."
                            raise RuntimeError(entry["error"])
                        entry["status"] = "captured"
                        write_report(output, results, args.sizes, args.theme)
                        print(f"[{index:3}/{len(args.sizes)}] {width}x{height}", flush=True)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        if results and results[-1]["status"] != "captured":
            results[-1]["error"] = str(error)
        write_report(output, results, args.sizes, args.theme)
        print(f"Failed: {error}\nPartial report: {output / 'index.html'}", flush=True)
        return 1
    write_report(output, results, args.sizes, args.theme)
    print(f"Gallery: {output / 'index.html'}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
