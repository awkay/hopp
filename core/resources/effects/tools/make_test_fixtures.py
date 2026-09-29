#!/usr/bin/env python3
"""Regenerates the tiny WebP fixtures used by the manifest validation tests
(core/src/effects/testdata/). Requires Pillow, img2webp and cwebp (libwebp).

    python3 core/resources/effects/tools/make_test_fixtures.py
"""

import os
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "..", "..", "src", "effects", "testdata")


def frame(width, height, index, opaque=False):
    background = (40, 40, 40, 255) if opaque else (0, 0, 0, 0)
    img = Image.new("RGBA", (width, height), background)
    draw = ImageDraw.Draw(img)
    inset = 2 + (index % 4)
    draw.rectangle((inset, inset, width - 1 - inset, height - 1 - inset),
                   fill=(255, 60 * (index % 4), 0, 255))
    return img


def animated(name, width, height, delays, opaque=False, workdir=None):
    args = [shutil.which("img2webp"), "-loop", "1", "-lossless"]
    for index, delay in enumerate(delays):
        path = os.path.join(workdir, f"{name}-{index}.png")
        frame(width, height, index, opaque).save(path)
        args += ["-d", str(delay), path]
    args += ["-o", os.path.join(OUT, name)]
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    for tool in ("img2webp", "cwebp"):
        if shutil.which(tool) is None:
            sys.exit(f"{tool} not found: install libwebp (brew install webp)")
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as workdir:
        animated("valid.webp", 64, 48, [40, 200, 40], workdir=workdir)
        animated("loop_1500.webp", 32, 32, [500, 500, 500], workdir=workdir)
        animated("canvas_too_wide.webp", 1290, 40, [100, 100], workdir=workdir)
        animated("canvas_too_tall.webp", 40, 730, [100, 100], workdir=workdir)
        animated("canvas_too_small.webp", 31, 64, [100, 100], workdir=workdir)
        animated("too_many_frames.webp", 32, 32, [20] * 73, workdir=workdir)
        animated("delay_10ms.webp", 32, 32, [100, 10, 100], workdir=workdir)
        animated("delay_1100ms.webp", 32, 32, [100, 1100], workdir=workdir)
        animated("loop_3100ms.webp", 32, 32, [1000, 1000, 1000, 100], workdir=workdir)
        animated("no_alpha.webp", 32, 32, [100, 100], opaque=True, workdir=workdir)
        still = os.path.join(workdir, "still.png")
        frame(32, 32, 0).save(still)
        subprocess.run([shutil.which("cwebp"), "-quiet", "-lossless", still, "-o",
                        os.path.join(OUT, "still.webp")], check=True)
    for name in sorted(os.listdir(OUT)):
        print(name, os.path.getsize(os.path.join(OUT, name)))


if __name__ == "__main__":
    main()
