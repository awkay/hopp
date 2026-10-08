#!/usr/bin/env python3
"""Shows the inclusive-time tree below a frame.

    drill.py viewer.folded.gz "process_audio_samples" 3

Takes the first frame in each stack containing the substring, and prints what it calls, down to
the given depth (default 3). Shares are of all samples; children under 0.3% are hidden.
"""

import sys
from collections import defaultdict

from buckets import read_folded


def main():
    path, needle = sys.argv[1], sys.argv[2]
    depth = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    total = matched = 0
    tree = defaultdict(int)
    for stack, weight in read_folded(path):
        total += weight
        frames = stack.split(";")
        index = next((i for i, frame in enumerate(frames) if needle in frame), None)
        if index is None:
            continue
        matched += weight
        below = frames[index + 1 : index + 1 + depth]
        for level in range(1, len(below) + 1):
            tree[tuple(below[:level])] += weight
    print(f"{needle}: {100 * matched / total:.1f}% of all samples")

    def show(prefix, level):
        children = sorted(
            ((path, w) for path, w in tree.items() if len(path) == level + 1 and path[:level] == prefix),
            key=lambda item: -item[1],
        )
        for path, weight in children:
            if 100 * weight / total < 0.3:
                continue
            print("  " * (level + 1) + f"{100 * weight / total:5.1f}%  {path[-1][:150]}")
            if level + 1 < depth:
                show(path, level + 1)

    show((), 0)


if __name__ == "__main__":
    main()
