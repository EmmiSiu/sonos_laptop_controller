#!/usr/bin/env python3
"""Generates the source artwork the Tauri bundler turns into platform icons.

Why a script rather than a checked-in binary: a 1 MB PNG in git that nobody can diff, edit, or
explain is worse than twenty lines that produce it. Run `just icon` after changing the mark,
then `npx tauri icon` to fan it out into every format the bundlers want.

The mark is the five-bar equaliser from the interface's idle state, in the same accent colour,
so the taskbar icon and the app agree with each other.

No dependencies: PNG is a container around zlib-compressed scanlines, and writing one by hand
is less code than justifying an image library in `deny.toml`.
"""

import pathlib
import struct
import sys
import zlib

SIZE = 1024

# Matches `tailwind.config.ts`: ink-900 behind, accent in front.
BACKGROUND = (0x0D, 0x11, 0x17)
ACCENT = (0x6E, 0xE7, 0xB7)

# (x centre, half-height) as fractions of the canvas, mirroring the SVG in `RadarIdle.vue`.
BARS = [(0.22, 0.14), (0.36, 0.24), (0.50, 0.33), (0.64, 0.24), (0.78, 0.14)]
BAR_HALF_WIDTH = 0.035
CORNER_RADIUS = 0.18


def rounded_square_alpha(x: float, y: float) -> bool:
    """Whether (x, y), in 0..1, is inside a squircle-ish rounded square."""
    r = CORNER_RADIUS
    cx = min(max(x, r), 1.0 - r)
    cy = min(max(y, r), 1.0 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def bar_covers(x: float, y: float) -> bool:
    """Whether (x, y) falls inside one of the equaliser bars."""
    for centre, half_height in BARS:
        if abs(x - centre) <= BAR_HALF_WIDTH and abs(y - 0.5) <= half_height:
            return True
    return False


def render() -> bytes:
    """Builds the raw RGBA scanlines, each prefixed with a PNG filter byte."""
    rows = bytearray()
    for py in range(SIZE):
        rows.append(0)  # filter type 0: none
        y = (py + 0.5) / SIZE
        for px in range(SIZE):
            x = (px + 0.5) / SIZE
            if not rounded_square_alpha(x, y):
                rows.extend((0, 0, 0, 0))
            elif bar_covers(x, y):
                rows.extend((*ACCENT, 255))
            else:
                rows.extend((*BACKGROUND, 255))
    return bytes(rows)


def chunk(tag: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + tag
        + payload
        + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
    )


def main() -> int:
    destination = pathlib.Path(
        sys.argv[1] if len(sys.argv) > 1 else "apps/desktop/src-tauri/icons/source.png"
    )
    destination.parent.mkdir(parents=True, exist_ok=True)

    header = struct.pack(">2I5B", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(render(), 9))
        + chunk(b"IEND", b"")
    )
    destination.write_bytes(png)
    print(f"wrote {destination} ({len(png) // 1024} KiB, {SIZE}x{SIZE})")
    print("now run: cd apps/desktop && npx tauri icon src-tauri/icons/source.png")
    return 0


if __name__ == "__main__":
    sys.exit(main())
