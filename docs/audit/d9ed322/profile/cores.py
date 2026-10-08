#!/usr/bin/env python3
"""Splits each thread's CPU between efficiency (E) and performance (P) cores.

    tar xzf viewer.trace.tar.gz
    xcrun xctrace export --input first-viewer.trace \
      --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' > viewer.xml
    cores.py viewer.xml

Needs the raw export: the folded stacks don't record which core a sample ran on.
"""

import re
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict


def main():
    values = {}  # xctrace writes each value once with id="n", then refers back with ref="n"
    by_thread = defaultdict(lambda: [0, 0])
    total = 0

    def value(element):
        return values.get(element.get("ref") or element.get("id"))

    for _, element in ET.iterparse(sys.argv[1], events=("end",)):
        key = element.get("id")
        if element.tag == "thread" and key:
            label = re.sub(r"\s+\(.*\)$", "", element.get("fmt", ""))
            values[key] = re.sub(r"\s+0x[0-9a-f]*", "", label)
        elif element.tag == "core" and key:
            values[key] = "E Core" in (element.get("fmt") or "")
        elif element.tag == "weight" and key:
            values[key] = int(element.text or 0)
        elif element.tag == "row":
            row = {child.tag: value(child) for child in element}
            weight = row.get("weight") or 0
            total += weight
            by_thread[row.get("thread")][0 if row.get("core") else 1] += weight
            element.clear()

    print("thread: share of all samples on E cores / P cores")
    for thread, (e_core, p_core) in sorted(by_thread.items(), key=lambda item: -sum(item[1]))[:10]:
        print(f"  {str(thread)[:40]:40} E {100 * e_core / total:5.1f}%  P {100 * p_core / total:5.1f}%")


if __name__ == "__main__":
    main()
