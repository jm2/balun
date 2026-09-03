#!/usr/bin/env python3
"""Render every raster form of Balun's application icon from the scalable SVG.

The SVG under ``data/icons/hicolor/scalable/apps`` is the only icon source. This
script renders it with librsvg (through GObject introspection) into:

- ``data/icons/hicolor/<size>x<size>/apps/io.github.jm2.Balun.png`` for the
  freedesktop sizes 16 to 512;
- ``data/balun.iconset/icon_<size>x<size>[@2x].png`` for the macOS icon set,
  whose largest member is 1024 px;
- ``data/balun.png``, a 1024 px master render;
- ``data/balun.ico`` with the Windows sizes 16 to 256.

Renders at 32 px and smaller hide SVG elements carrying ``class="fine"`` and
thicken the rabbit ears so the small icons keep their silhouette instead of
turning into noise. Runs offline
and needs ``python3-gobject``, ``python3-cairo``, ``librsvg`` with its
typelib, and Pillow (for the ``.ico`` container only).

Usage: ``python3 scripts/render-icons.py [--check]``. ``--check`` renders into
memory and reports every committed raster whose bytes differ, without writing.
Renders are deterministic for one librsvg and cairo version; regenerate on the
same host you compared on.
"""

from __future__ import annotations

import argparse
import io
import sys
from pathlib import Path

import gi

gi.require_version("Rsvg", "2.0")
import cairo  # noqa: E402
from gi.repository import Rsvg  # noqa: E402

REPOSITORY = Path(__file__).resolve().parent.parent
APP_ID = "io.github.jm2.Balun"
SOURCE = REPOSITORY / "data/icons/hicolor/scalable/apps" / f"{APP_ID}.svg"
HICOLOR_SIZES = (16, 24, 32, 48, 64, 128, 256, 512)
ICONSET = (
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
)
MASTER_SIZE = 1024
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
SMALL_LIMIT = 32
SMALL_STYLESHEET = b"""
.fine { display: none; }
.ear-outer { stroke-width: 8.4; }
.ear-outer-thin { stroke-width: 6.6; }
.ear-metal { stroke-width: 5.6; }
.ear-metal-thin { stroke-width: 4; }
"""


class Renderer:
    """Render one SVG document at any square size, with a small-size variant."""

    def __init__(self, source: Path) -> None:
        self.detailed = Rsvg.Handle.new_from_file(str(source))
        self.simplified = Rsvg.Handle.new_from_file(str(source))
        self.simplified.set_stylesheet(SMALL_STYLESHEET)
        self.cache: dict[int, bytes] = {}

    def png(self, size: int) -> bytes:
        if size not in self.cache:
            handle = self.simplified if size <= SMALL_LIMIT else self.detailed
            surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, size, size)
            context = cairo.Context(surface)
            viewport = Rsvg.Rectangle()
            viewport.x = 0.0
            viewport.y = 0.0
            viewport.width = float(size)
            viewport.height = float(size)
            handle.render_document(context, viewport)
            surface.flush()
            buffer = io.BytesIO()
            surface.write_to_png(buffer)
            self.cache[size] = buffer.getvalue()
        return self.cache[size]

    def ico(self) -> bytes:
        from PIL import Image

        frames = {size: Image.open(io.BytesIO(self.png(size))) for size in ICO_SIZES}
        largest = max(ICO_SIZES)
        buffer = io.BytesIO()
        frames[largest].save(
            buffer,
            format="ICO",
            sizes=[(size, size) for size in ICO_SIZES],
            append_images=[frames[size] for size in ICO_SIZES if size != largest],
        )
        return buffer.getvalue()


def outputs(renderer: Renderer) -> dict[Path, bytes]:
    files: dict[Path, bytes] = {}
    for size in HICOLOR_SIZES:
        files[REPOSITORY / f"data/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"] = renderer.png(size)
    for name, size in ICONSET:
        files[REPOSITORY / "data/balun.iconset" / name] = renderer.png(size)
    files[REPOSITORY / "data/balun.png"] = renderer.png(MASTER_SIZE)
    files[REPOSITORY / "data/balun.ico"] = renderer.ico()
    return files


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="compare instead of writing")
    arguments = parser.parse_args()

    renderer = Renderer(SOURCE)
    stale = []
    for path, data in outputs(renderer).items():
        if arguments.check:
            if not path.exists() or path.read_bytes() != data:
                stale.append(path)
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        print(f"wrote {path.relative_to(REPOSITORY)} ({len(data)} bytes)")

    if stale:
        for path in stale:
            print(f"stale: {path.relative_to(REPOSITORY)}", file=sys.stderr)
        return 1
    if arguments.check:
        print("every raster matches the SVG render")
    return 0


if __name__ == "__main__":
    sys.exit(main())
