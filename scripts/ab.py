#!/usr/bin/env python3
"""A/B two builds of a2600-bench on this shared machine: alternate them, game
by game, on one pinned CPU, and compare medians.

    ab.py BIN_A BIN_B [--rounds N] [--frames N] [--cpu N]

Each round runs A then B (then B then A the next round) on each game; the
result is B's best speed over A's best, per game and overall (the geometric
mean). The best, not the median: on this shared machine other work only ever
slows a run down (a busy SMT sibling, shared cache), so each side's fastest
of many short runs is the closest to its own speed. The checksums must agree, or the builds do not compute
the same thing and the comparison is refused. An A/A run shows the noise.
"""

from __future__ import annotations

import argparse
import re
import statistics
import subprocess
import sys

GAMES = [
    "/data/emu/roms/atari2600/Combat (Europe).zip",
    "/data/emu/roms/atari2600/Pitfall! - Pitfall Harry's Jungle Adventure (USA).zip",
    "/data/emu/roms/atari2600/River Raid (USA).zip",
    "/data/emu/roms/atari2600/David Crane's Pitfall II - Lost Caverns (USA).zip",
    "/data/emu/roms/atari2600/Space Invaders (USA).zip",
]
LINE = re.compile(r"([\d.]+) frames/s, check ([0-9A-F]{8})")


def bench(binary: str, game: str, frames: int, cpu: int) -> tuple[float, str]:
    out = subprocess.run(
        ["taskset", "-c", str(cpu), binary, game, "--frames", str(frames)],
        capture_output=True, text=True, check=True,
    ).stdout
    fps, check = LINE.search(out).groups()
    return float(fps), check


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("a")
    parser.add_argument("b")
    parser.add_argument("--rounds", type=int, default=12)
    parser.add_argument("--frames", type=int, default=100)
    parser.add_argument("--cpu", type=int, default=31)
    args = parser.parse_args()
    ratios = []
    for game in GAMES:
        a_runs, b_runs = [], []
        for r in range(args.rounds):
            order = [("a", args.a), ("b", args.b)] if r % 2 == 0 else [("b", args.b), ("a", args.a)]
            for side, binary in order:
                fps, check = bench(binary, game, args.frames, args.cpu)
                (a_runs if side == "a" else b_runs).append((fps, check))
        checks = {c for _, c in a_runs} | {c for _, c in b_runs}
        name = game.rsplit("/", 1)[-1].removesuffix(".zip")
        if len(checks) != 1:
            print(f"{name}: checksums differ {sorted(checks)}: the builds are not equivalent")
            return 1
        a = max(f for f, _ in a_runs)
        b = max(f for f, _ in b_runs)
        ratios.append(b / a)
        print(f"{name}: A {a:.1f} fps, B {b:.1f} fps, x{b / a:.3f}", flush=True)
    geo = statistics.geometric_mean(ratios)
    print(f"overall x{geo:.3f} (geometric mean of {len(ratios)} games, {args.rounds} rounds each)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
