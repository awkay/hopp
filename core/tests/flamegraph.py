#!/usr/bin/env python3
"""Turns an Instruments Time Profiler export into a flamegraph and a hot-function summary.

    flamegraph.py <time-profile.xml> <out-prefix> [--title TEXT] [--seconds N] [--demangler CMD]

The XML is `xctrace export --xpath '.../table[@schema="time-profile"]'` (profile.sh does it).
Writes:
  <out-prefix>.folded  one "thread;frame;...;leaf <microseconds>" line per stack, the input
                       format of flamegraph.pl, inferno and speedscope
  <out-prefix>.svg     the flamegraph; hover a frame for its name and share
  <out-prefix>.txt     CPU per thread and the functions with the most self and total time
                       (also printed)
Time Profiler samples only threads that are on a CPU, so every number here is CPU time.
Instruments leaves Rust v0 symbols (`_RNv…`) mangled; --demangler names a command that reads
symbols on stdin and writes them demangled, one per line (`hopp_core_tests demangle`).
"""

import argparse
import html
import re
import shlex
import subprocess
import xml.etree.ElementTree as ET
from collections import defaultdict

# Legacy Rust mangling escapes that Instruments leaves in place.
RUST_ESCAPES = {
    "$LT$": "<", "$GT$": ">", "$RF$": "&", "$BP$": "*", "$C$": ",", "$SP$": "@",
    "$u20$": " ", "$u22$": '"', "$u27$": "'", "$u2b$": "+", "$u3b$": ";",
    "$u5b$": "[", "$u5d$": "]", "$u7b$": "{", "$u7d$": "}", "$u7e$": "~",
}
OUR_CRATES = ("hopp_core", "socket_lib", "sentry_utils", "<hopp_core")
CPP_MEDIA = ("webrtc", "rtc::", "cricket", "libyuv", "dcsctp", "absl::", "bssl", "Yuv", "I420")


def clean(name):
    name = re.sub(r"::h[0-9a-f]{16}$", "", name)
    for escape, char in RUST_ESCAPES.items():
        name = name.replace(escape, char)
    # ';' separates frames in the folded format.
    return name.replace("..", "::").replace(";", ":")


def thread_label(fmt):
    # "network_thread 0x6000 0x1a2b3 (hopp_core, pid: 123)" -> "network_thread"; unnamed
    # threads show as the process name.
    label = re.sub(r"\s+\(.*\)$", "", fmt)
    return clean(re.sub(r"\s+0x[0-9a-f]*", "", label)) or "thread"


def read_samples(path):
    """Yields (thread, [(frame, binary)] root-first, weight in ns) per sample."""
    # xctrace writes each value once with id="n" and points back to it with ref="n" later.
    values = {}

    def value(element):
        return values.get(element.get("ref") or element.get("id"))

    for _, element in ET.iterparse(path, events=("end",)):
        tag, key = element.tag, element.get("id")
        if key is None and tag != "row":
            continue
        if tag == "binary":
            values[key] = element.get("name", "?")
        elif tag == "frame":
            binary = next((value(c) for c in element if c.tag == "binary"), "?")
            values[key] = (element.get("name", "?"), binary)
        elif tag == "backtrace":
            values[key] = [value(c) for c in reversed(element) if c.tag == "frame"]
        elif tag == "thread":
            values[key] = thread_label(element.get("fmt", ""))
        elif tag == "weight":
            values[key] = int(element.text or 0)
        elif tag == "row":
            row = {child.tag: value(child) for child in element}
            if row.get("weight"):
                yield row.get("thread") or "thread", row.get("backtrace") or [], row["weight"]
            element.clear()


def demangle(names, command):
    """Maps each raw symbol to its readable name."""
    names = sorted(names)
    if command:
        result = subprocess.run(
            shlex.split(command), input="\n".join(names) + "\n",
            capture_output=True, text=True, check=True,
        )
        demangled = result.stdout.splitlines()
        if len(demangled) == len(names):
            return {raw: clean(name) for raw, name in zip(names, demangled)}
    return {raw: clean(raw) for raw in names}


def kind(name, binary):
    if not binary.startswith("hopp_core"):
        return "system"
    if name.startswith(OUR_CRATES):
        return "ours"
    if any(marker in name for marker in CPP_MEDIA):
        return "media"
    return "rust"


COLORS = {  # (hue, saturation, base lightness)
    "ours": (18, 85, 58),
    "rust": (45, 85, 58),
    "media": (205, 60, 62),
    "system": (0, 0, 74),
    "thread": (0, 0, 86),
}


def color(name, frame_kind):
    hue, saturation, lightness = COLORS[frame_kind]
    lightness += hash(name) % 10 - 5  # neighbours stay distinguishable
    return f"hsl({hue},{saturation}%,{lightness}%)"


def render_svg(stacks, kinds, total, title, path):
    root = {"children": {}, "weight": 0}
    depth = 0
    for frames, weight in stacks.items():
        node = root
        node["weight"] += weight
        for name in frames:
            node = node["children"].setdefault(name, {"children": {}, "weight": 0})
            node["weight"] += weight
        depth = max(depth, len(frames))

    width, row, top = 1800, 17, 44
    height = top + depth * row + 10
    scale = (width - 20) / max(total, 1)
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'font-family="Menlo,monospace" font-size="11">',
        f'<rect width="100%" height="100%" fill="#fbfbf8"/>',
        f'<text x="10" y="22" font-size="15">{html.escape(title)}</text>',
        '<text x="10" y="38" fill="#666">hover for details · orange: hopp_core · '
        "yellow: other Rust · blue: libwebrtc/libyuv · grey: macOS</text>",
    ]

    def draw(node, name, x, level):
        w = node["weight"] * scale
        if w < 0.4:
            return
        y = height - 10 - (level + 1) * row
        share = 100 * node["weight"] / total
        fill = color(name, kinds.get(name, "thread"))
        label = html.escape(name)
        parts.append(
            f'<g><title>{label} ({share:.2f}%, {node["weight"] / 1e6:.1f} ms)</title>'
            f'<rect x="{x:.1f}" y="{y}" width="{w:.1f}" height="{row - 1}" fill="{fill}" rx="2"/>'
        )
        chars = int((w - 6) / 6.7)
        if chars >= 3:
            text = name if len(name) <= chars else name[: chars - 1] + "…"
            parts.append(f'<text x="{x + 3:.1f}" y="{y + 12}">{html.escape(text)}</text>')
        parts.append("</g>")
        child_x = x
        for child_name, child in sorted(node["children"].items()):
            draw(child, child_name, child_x, level + 1)
            child_x += child["weight"] * scale

    x = 10.0
    for name, child in sorted(root["children"].items()):
        draw(child, name, x, 0)
        x += child["weight"] * scale
    parts.append("</svg>")
    with open(path, "w") as out:
        out.write("\n".join(parts))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("xml")
    parser.add_argument("out_prefix")
    parser.add_argument("--title", default="hopp_core")
    parser.add_argument("--seconds", type=float, help="recording length, for average CPU")
    parser.add_argument("--demangler", help="command that demangles symbols on stdin")
    args = parser.parse_args()

    raw_stacks = defaultdict(int)  # (thread, (raw symbol, binary), ...) -> ns
    for thread, frames, weight in read_samples(args.xml):
        raw_stacks[(thread, *frames)] += weight
    readable = demangle({raw for stack in raw_stacks for raw, _ in stack[1:]}, args.demangler)

    stacks = defaultdict(int)  # (thread, frame, ...) -> ns
    self_time = defaultdict(int)
    total_time = defaultdict(int)
    per_thread = defaultdict(int)
    kinds = {}
    binaries = {}
    total = 0
    for (thread, *raw_frames), weight in raw_stacks.items():
        frames = [(readable[raw], binary) for raw, binary in raw_frames]
        names = [name for name, _ in frames]
        stacks[(thread, *names)] += weight
        per_thread[thread] += weight
        total += weight
        for name, binary in frames:
            kinds[name] = kind(name, binary)
            binaries[name] = binary
        if names:
            self_time[names[-1]] += weight
        for name in set(names):
            total_time[name] += weight
    if not total:
        raise SystemExit(f"no samples in {args.xml}: was the process idle or already gone?")

    with open(f"{args.out_prefix}.folded", "w") as out:
        for frames, weight in sorted(stacks.items()):
            out.write(f"{';'.join(frames)} {weight // 1000}\n")
    render_svg(stacks, kinds, total, args.title, f"{args.out_prefix}.svg")

    pct = lambda ns: 100 * ns / total  # noqa: E731
    lines = [args.title]
    cpu = f"{total / 1e9:.1f} CPU-seconds sampled"
    if args.seconds:
        cpu += f" over {args.seconds:.0f} s = {100 * total / 1e9 / args.seconds:.0f}% of one core"
    lines += [cpu, "", "CPU by thread"]
    for thread, ns in sorted(per_thread.items(), key=lambda item: -item[1])[:15]:
        lines.append(f"  {pct(ns):5.1f}%  {thread}")
    lines += ["", "Most self time (the function itself, not what it calls)"]
    for name, ns in sorted(self_time.items(), key=lambda item: -item[1])[:30]:
        lines.append(f"  {pct(ns):5.1f}%  {name}  [{binaries.get(name, '?')}]")
    lines += ["", "Most total time in hopp_core's own functions (including what they call)"]
    ours = [(n, ns) for n, ns in total_time.items() if kinds.get(n) == "ours"]
    for name, ns in sorted(ours, key=lambda item: -item[1])[:30]:
        lines.append(f"  {pct(ns):5.1f}%  {name}")
    report = "\n".join(lines)
    with open(f"{args.out_prefix}.txt", "w") as out:
        out.write(report + "\n")
    print(report)


if __name__ == "__main__":
    main()
