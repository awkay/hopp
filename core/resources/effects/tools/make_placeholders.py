#!/usr/bin/env python3
"""Regenerates the placeholder screen effects in core/resources/effects/.

Requires Python 3 with Pillow (`pip install pillow`) and `img2webp` from libwebp
(`brew install webp`, `apt install webp`). Output is deterministic for a given
Pillow + libwebp version.

    python3 core/resources/effects/tools/make_placeholders.py

Each effect is drawn as a PNG sequence (supersampled 2x for anti-aliasing), then
packed with img2webp using an explicit per-frame duration (`-d`), lossy colour at
q=80 and lossless alpha. The placeholders deliberately use uneven delays and at
least one long held frame so the player's timing is exercised.
"""

import math
import os
import random
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_DIR = os.path.dirname(HERE)
SS = 2  # supersampling factor


def canvas(width, height):
    return Image.new("RGBA", (width * SS, height * SS), (0, 0, 0, 0))


def finish(image, width, height):
    return image.resize((width, height), Image.LANCZOS)


def star_points(cx, cy, outer, inner, rotation=-math.pi / 2, sx=1.0, sy=1.0):
    points = []
    for i in range(10):
        radius = outer if i % 2 == 0 else inner
        angle = rotation + i * math.pi / 5
        points.append((cx + math.cos(angle) * radius * sx, cy + math.sin(angle) * radius * sy))
    return points


def with_alpha(image, alpha):
    """Scales the alpha channel of an RGBA image by `alpha` (0..1)."""
    if alpha >= 1.0:
        return image
    r, g, b, a = image.split()
    a = a.point(lambda v: int(v * alpha + 0.5))
    return Image.merge("RGBA", (r, g, b, a))


# ── Star bounce: large (960x540) ─────────────────────────────────────────────


def star_bounce():
    width, height = 960, 540
    frames = []
    floor = height - 90
    top = 110
    radius = 80

    def frame(y, squash=1.0, spin=0.0, alpha=1.0):
        img = canvas(width, height)
        draw = ImageDraw.Draw(img)
        cx = width / 2
        # Soft shadow that grows as the star nears the floor.
        closeness = max(0.0, min(1.0, (y - top) / (floor - top)))
        shadow_w = (60 + 70 * closeness) * SS
        shadow_h = (10 + 8 * closeness) * SS
        shadow_a = int((50 + 90 * closeness) * alpha)
        draw.ellipse(
            (cx * SS - shadow_w, (floor + radius) * SS - shadow_h,
             cx * SS + shadow_w, (floor + radius) * SS + shadow_h),
            fill=(0, 0, 0, shadow_a),
        )
        sx, sy = 1.0 / squash, squash
        cy = y + radius * (1 - sy)
        outline = star_points(cx * SS, cy * SS, (radius + 8) * SS, (radius * 0.45 + 6) * SS,
                              -math.pi / 2 + spin, sx, sy)
        body = star_points(cx * SS, cy * SS, radius * SS, radius * 0.45 * SS,
                           -math.pi / 2 + spin, sx, sy)
        draw.polygon(outline, fill=(170, 90, 0, int(255 * alpha)))
        draw.polygon(body, fill=(255, 205, 40, int(255 * alpha)))
        # Highlight.
        draw.ellipse(
            ((cx - 30) * SS, (cy - 40) * SS, (cx - 8) * SS, (cy - 18) * SS),
            fill=(255, 250, 210, int(200 * alpha)),
        )
        return finish(img, width, height)

    # Fall, squash (held), bounce up, slow apex (held), fall, small bounce, fade.
    def fall(y0, y1, steps, delay, spin0, spin1):
        for i in range(1, steps + 1):
            t = i / steps
            y = y0 + (y1 - y0) * t * t
            frames.append((frame(y, spin=spin0 + (spin1 - spin0) * t), delay))

    def rise(y0, y1, steps, delay, spin0, spin1):
        for i in range(1, steps + 1):
            t = i / steps
            y = y0 + (y1 - y0) * (1 - (1 - t) * (1 - t))
            frames.append((frame(y, spin=spin0 + (spin1 - spin0) * t), delay))

    frames.append((frame(top), 120))  # held at the start
    fall(top, floor, 8, 30, 0.0, 0.6)
    frames.append((frame(floor, squash=0.7, spin=0.6), 140))  # held squash
    rise(floor, top + 60, 7, 35, 0.6, 1.2)
    frames.append((frame(top + 60, spin=1.2), 300))  # held apex
    fall(top + 60, floor, 7, 30, 1.2, 1.8)
    frames.append((frame(floor, squash=0.8, spin=1.8), 90))
    rise(floor, floor - 120, 4, 40, 1.8, 2.0)
    fall(floor - 120, floor, 4, 40, 2.0, 2.2)
    for i, alpha in enumerate((0.75, 0.5, 0.25, 0.08)):
        frames.append((frame(floor, spin=2.2, alpha=alpha), 60))
    return "star_bounce.webp", frames


# ── Thumbs up: small (256x256) ───────────────────────────────────────────────


def thumbs_up():
    size = 256

    def badge(scale, alpha):
        img = canvas(size, size)
        draw = ImageDraw.Draw(img)
        c = size / 2 * SS

        def s(v):
            return v * scale * SS

        # Badge.
        draw.ellipse((c - s(118), c - s(118), c + s(118), c + s(118)), fill=(20, 60, 140, 255))
        draw.ellipse((c - s(108), c - s(108), c + s(108), c + s(108)), fill=(45, 120, 235, 255))
        # Thumb (upright rounded bar) and fist (rounded block with finger lines).
        white = (255, 255, 255, 255)
        draw.rounded_rectangle((c - s(38), c - s(78), c + s(2), c + s(0)), radius=s(20), fill=white)
        draw.rounded_rectangle((c - s(52), c - s(14), c + s(58), c + s(72)), radius=s(18), fill=white)
        draw.rounded_rectangle((c - s(84), c - s(10), c - s(58), c + s(72)), radius=s(8), fill=white)
        for i in range(3):
            y = c + s(8 + i * 22)
            draw.line((c + s(8), y, c + s(56), y), fill=(45, 120, 235, 255), width=max(1, int(s(5))))
        return finish(with_alpha(img, alpha), size, size)

    frames = []
    # Scale in with overshoot (uneven delays), a long hold, then fade out.
    for scale, delay in ((0.2, 30), (0.45, 30), (0.7, 30), (0.95, 30), (1.08, 40), (1.02, 40), (0.98, 40)):
        frames.append((badge(scale, 1.0), delay))
    frames.append((badge(1.0, 1.0), 800))  # held frame
    for alpha, delay in ((0.8, 50), (0.6, 50), (0.4, 50), (0.2, 50), (0.05, 50)):
        frames.append((badge(1.0 - (1.0 - alpha) * 0.1, alpha), delay))
    return "thumbs_up.webp", frames


# ── Confetti: medium (640x360), plays twice ──────────────────────────────────


def confetti():
    width, height = 640, 360
    rng = random.Random(1234)
    colours = [(239, 68, 68), (250, 204, 21), (34, 197, 94), (59, 130, 246), (168, 85, 247), (236, 72, 153)]
    pieces = []
    for _ in range(46):
        pieces.append({
            "x": rng.uniform(40, width - 40),
            "y": rng.uniform(-80, 40),
            "vx": rng.uniform(-60, 60),
            "vy": rng.uniform(40, 140),
            "spin": rng.uniform(-6, 6),
            "angle": rng.uniform(0, math.pi),
            "w": rng.uniform(8, 16),
            "h": rng.uniform(4, 8),
            "colour": rng.choice(colours),
        })

    steps = 22
    frames = []
    for i in range(steps):
        t = i * 0.06
        alpha = 1.0 if i < steps - 5 else (steps - i) / 6.0
        img = canvas(width, height)
        draw = ImageDraw.Draw(img)
        for p in pieces:
            x = p["x"] + p["vx"] * t
            y = p["y"] + p["vy"] * t + 0.5 * 220 * t * t
            angle = p["angle"] + p["spin"] * t
            flip = abs(math.cos(angle * 1.7))
            hw, hh = p["w"] / 2, p["h"] / 2 * (0.3 + 0.7 * flip)
            ca, sa = math.cos(angle), math.sin(angle)
            corners = [
                ((x + dx * ca - dy * sa) * SS, (y + dx * sa + dy * ca) * SS)
                for dx, dy in ((-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh))
            ]
            draw.polygon(corners, fill=p["colour"] + (int(255 * alpha),))
        # Uneven delays: quick burst, slower drift, one held beat in the middle.
        delay = 40 if i < 8 else (240 if i == 11 else 60)
        frames.append((finish(img, width, height), delay))
    return "confetti.webp", frames


def encode(name, frames, workdir):
    img2webp = shutil.which("img2webp")
    if img2webp is None:
        sys.exit("img2webp not found: install libwebp (brew install webp)")
    args = [img2webp, "-loop", "1", "-lossy", "-q", "80", "-m", "6"]
    for index, (image, delay) in enumerate(frames):
        path = os.path.join(workdir, f"{name}-{index:03d}.png")
        image.save(path)
        args += ["-d", str(delay), path]
    out = os.path.join(OUT_DIR, name)
    args += ["-o", out]
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)
    total = sum(delay for _, delay in frames)
    print(f"{name}: {len(frames)} frames, {total} ms, {os.path.getsize(out)} bytes")


def main():
    with tempfile.TemporaryDirectory() as workdir:
        for make in (star_bounce, thumbs_up, confetti):
            name, frames = make()
            encode(name, frames, workdir)


if __name__ == "__main__":
    main()
