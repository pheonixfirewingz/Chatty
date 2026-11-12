#!/usr/bin/env python3
"""Regenerate the Windows .ico and macOS .icns from the Flatpak SVG icon.

Needs rsvg-convert (librsvg). Run from the repository root:

    python3 scripts/make-icons.py
"""

import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SVG = ROOT / "packaging/flatpak/io.github.pheonixfirewingz.Chatty.svg"
ICO = ROOT / "packaging/windows/chatty.ico"
ICNS = ROOT / "packaging/macos/chatty.icns"

ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
# PNG-capable icns element types and their pixel sizes.
ICNS_TYPES = [
    (b"icp4", 16), (b"icp5", 32), (b"ic11", 32), (b"ic12", 64),
    (b"ic07", 128), (b"ic13", 256), (b"ic08", 256), (b"ic14", 512),
    (b"ic09", 512), (b"ic10", 1024),
]


def png(size: int) -> bytes:
    return subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), str(SVG)],
        check=True, capture_output=True,
    ).stdout


def write_ico() -> None:
    images = [png(size) for size in ICO_SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, image in zip(ICO_SIZES, images):
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(image), offset)
        offset += len(image)
        data += image
    ICO.write_bytes(header + entries + data)


def write_icns() -> None:
    cache: dict[int, bytes] = {}
    body = b""
    for kind, size in ICNS_TYPES:
        image = cache.setdefault(size, png(size))
        body += kind + struct.pack(">I", 8 + len(image)) + image
    ICNS.write_bytes(b"icns" + struct.pack(">I", 8 + len(body)) + body)


if __name__ == "__main__":
    write_ico()
    write_icns()
    print(f"wrote {ICO.relative_to(ROOT)} and {ICNS.relative_to(ROOT)}", file=sys.stderr)
