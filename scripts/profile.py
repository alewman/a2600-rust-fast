#!/usr/bin/env python3
"""Fold a2600-profile's samples into functions and source lines with addr2line.

    profile.py [external/profile.txt] [--lines N]

Each sample's address is resolved with its inline chain (``addr2line -i``):
the innermost frame is where the time went, the outer ones where it was
called from. Prints the functions by inclusive share, then the hottest lines.
"""

from __future__ import annotations

import collections
import subprocess
import sys


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    lines_wanted = 40
    if "--lines" in sys.argv:
        lines_wanted = int(sys.argv[sys.argv.index("--lines") + 1])
        args = [a for a in args if a != str(lines_wanted)]
    path = args[0] if args else "external/profile.txt"
    exe, *addresses = open(path).read().split()
    counts = collections.Counter(addresses)
    unique = list(counts)
    out = subprocess.run(
        ["addr2line", "-e", exe, "-f", "-i", "-C", *("0x" + a for a in unique)],
        capture_output=True, text=True, check=True,
    ).stdout.splitlines()
    # addr2line -i prints (function, file:line) pairs, innermost first, and
    # one more pair per inlined caller; re-run per address to split them.
    total = sum(counts.values())
    functions: collections.Counter = collections.Counter()
    where: collections.Counter = collections.Counter()
    for address in unique:
        chain = subprocess.run(
            ["addr2line", "-e", exe, "-f", "-i", "-C", "0x" + address],
            capture_output=True, text=True, check=True,
        ).stdout.splitlines()
        pairs = list(zip(chain[0::2], chain[1::2]))
        n = counts[address]
        seen = set()
        for function, _ in pairs:
            name = function.split("::")[-2:] if "::" in function else [function]
            name = "::".join(name)
            if name not in seen:
                functions[name] += n
                seen.add(name)
        if pairs:
            where[pairs[0][1].split("/src/")[-1].split(" ")[0]] += n
    print(f"{total} samples")
    print("\nfunctions (inclusive):")
    for name, n in functions.most_common(30):
        print(f"  {100 * n / total:5.1f}%  {name}")
    print("\nlines (self):")
    for line, n in where.most_common(lines_wanted):
        print(f"  {100 * n / total:5.1f}%  {line}")
    del out
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
