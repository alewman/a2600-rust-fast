#!/usr/bin/env python3
"""A gate that cannot fail proves nothing: build the board with one fault at a
time and check that some rung catches each.

Each fault is one exact text replacement in a copy of ``src/``, built into its
own target directory; then rung 1 (the die traces) and the lockstep cases run
against it, the reference's traces taken from the cache a full gate run left
(``lockstep.py --reference-cache``), until one fails. A fault no rung catches
is reported as MISSED.

    seeded_faults.py [--cache DIR] [--only NAME]
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# name: (file, exact text, replacement)
FAULTS = {
    # The TIA's rules the die settled (rung 1 should catch these).
    "c25-colour-a-step-late": (
        "tia.rs",
        "            if (COLUP0..=COLUBK).contains(&address) {\n                self.out_colours[(address - COLUP0) as usize] = write.unwrap().1;\n            }\n",
        "",
    ),
    "c24-vblank-not-gating-collisions": (
        "tia.rs",
        "} else if rise && !hblank && !self.vblank_latched {",
        "} else if rise && !hblank {",
    ),
    "c27-latch-window-a-step-short": (
        "tia.rs",
        "let n_hmove_latch = if h2 && self.hmove_history & 0xFF0 != 0 {",
        "let n_hmove_latch = if h2 && self.hmove_history & 0xFE0 != 0 {",
    ),
    "c28-stuffed-pulse-lost": (
        "tia.rs",
        "if stuff & kept & (0x10 >> i) != 0 && (c.strobe_left != 0 || held) {",
        "if stuff & kept & (0x10 >> i) != 0 && c.strobe_left != 0 {",
    ),
    "c26-lockup-guard": (
        "audio.rs",
        "            _ => pulse & 0x02 == 0,",
        # %0000 held instead of stepping to %1111, as a guard term would
        "            _ => pulse & 0x02 == 0 && pulse != 0,",
    ),
    "c29-playfield-from-hblank": (
        "tia.rs",
        "        let pf_pixel = self.pf_shown && !hblank;\n        if h1 {\n            let mut index_next = if hcount == RHB {",
        "        let pf_pixel = self.pf_shown && !hblank;\n        if h1 {\n            let mut index_next = if hcount == (if self.hmove_latch { LRHB } else { RHB }) {",
    ),
    # A console's revision (rung 2's #699 cases should catch it).
    "revision-merged-pulse-ignored": (
        "tia.rs",
        "let object_motck = (if motck { 0x1F & !merged } else { 0 }) | (merge & steps);",
        "let object_motck = (if motck { 0x1F & !merged } else { 0 }) | (merge & steps | merge);",
    ),
    # The RIOT's timer (C6).
    "c6-write-on-wrap-clears": (
        "riot.rs",
        "        self.as_of = cycle;\n        if self.wrapped_at != cycle {\n            self.flags &= !TIMER_FLAG;\n        }\n    }\n\n    // -- the bus",
        "        self.as_of = cycle;\n        self.flags &= !TIMER_FLAG;\n    }\n\n    // -- the bus",
    ),
    "c6-read-on-wrap-clears": (
        "riot.rs",
        "            if self.wrapped_at != cycle {\n                self.flags &= !TIMER_FLAG;\n            }\n            return self.value;",
        "            self.flags &= !TIMER_FLAG;\n            return self.value;",
    ),
    # Schemes.
    "fe-latches-d7": (
        "cartridge/activision.rs",
        "self.bank = if value & 0x20 != 0 { 0 } else { 1 };",
        "self.bank = if value & 0x80 != 0 { 0 } else { 1 };",
    ),
    "3f-arms-on-a7-only": (
        "cartridge/tigervision.rs",
        "self.armed = address & 0x10C0 == 0;",
        "self.armed = address & 0x1080 == 0;",
    ),
    "dpc-rng-feedback": (
        "cartridge/dpc.rs",
        "let bit = !((value >> 7) ^ (value >> 5) ^ (value >> 4) ^ (value >> 3)) & 1;",
        "let bit = !((value >> 7) ^ (value >> 5) ^ (value >> 4) ^ (value >> 2)) & 1;",
    ),
    "ar-write-on-fourth-change": (
        "cartridge/supercharger.rs",
        "            if self.changes == 5 {",
        "            if self.changes == 4 {",
    ),
    # The fork's shortcuts: each must be caught, or it could diverge unseen.
    "fast-quiet-step-drops-nusiz-history": (
        "tia.rs",
        "                p.size_history = (self.nusiz[i], p.size_history.0);\n",
        "",
    ),
    "fast-pixel-cache-kept-over-a-write": (
        "tia.rs",
        "            self.write_register(address, value);\n            self.pixels_cache = None;\n",
        "            self.write_register(address, value);\n",
    ),
    "fast-plain-step-keeps-pending": (
        "tia.rs",
        "                    c.pending = false;\n",
        "",
    ),
    "fast-plain-step-taken-under-a-lock": (
        "tia.rs",
        "            && !locks[2]\n",
        "",
    ),
    "fast-busy-ignores-the-hold": (
        "tia.rs",
        ".any(|c| c.strobe_left != 0 || c.window_history != 0 || c.deferred);",
        ".any(|c| c.strobe_left != 0 || c.deferred);",
    ),
    # The board: a halted (WSYNC) cycle a half clock short. (Sampling RDY on
    # another of steps one to five is not a fault: RDY only changes on a
    # cycle's first or last step, board.rs.)
    "wsync-halted-cycle-short": (
        "board.rs",
        "            // a halted cycle: the 6507 repeats this read\n            self.tia.step(None);\n            self.tia.step(None);\n            self.tia.step(None);",
        "            // a halted cycle: the 6507 repeats this read\n            self.tia.step(None);\n            self.tia.step(None);",
    ),
}


def run(command: list[str]) -> bool:
    return subprocess.run(["nice", *command], capture_output=True, text=True).returncode == 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--cache", type=Path, default=ROOT / "external" / "refcache")
    parser.add_argument("--work", type=Path, default=ROOT / "external" / "faults")
    parser.add_argument("--only")
    parser.add_argument("--jobs", type=int, default=12)
    args = parser.parse_args()
    traces = sorted(str(p) for p in (ROOT / "external" / "tia-fpga" / "sim" / "traces").rglob("*.trace"))
    local = Path("/data/emu/a2600-python/tests/die_traces")
    if local.is_dir():
        traces += sorted(str(p) for p in local.rglob("*.trace"))
    missed = ran = 0
    for name, (file, old, new) in FAULTS.items():
        if args.only and name != args.only:
            continue
        ran += 1
        tree = args.work / name
        if tree.exists():
            shutil.rmtree(tree)
        shutil.copytree(ROOT / "src", tree / "src")
        for extra in ("Cargo.toml", "Cargo.lock"):
            shutil.copy(ROOT / extra, tree / extra)
        source = tree / "src" / file
        text = source.read_text()
        if text.count(old) != 1:
            print(f"{name}: the fault's text is not in {file} exactly once")
            return 2
        source.write_text(text.replace(old, new))
        target = args.work / "target"
        build = subprocess.run(
            ["nice", "cargo", "build", "--release", "-q", "--manifest-path", str(tree / "Cargo.toml"),
             "--target-dir", str(target)],
            capture_output=True, text=True,
        )  # fmt: skip
        if build.returncode:
            print(f"{name}: does not build\n{build.stderr[-800:]}")
            return 2
        binaries = target / "release"
        caught = None
        if not run([str(binaries / "die-trace"), *traces]):
            caught = "rung 1 (the die)"
        else:
            lockstep = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "lockstep.py"), "--rust",
                 str(binaries / "a2600-trace"), "--reference-cache", str(args.cache),
                 "--traces", str(args.work / "traces" / name), "--jobs", str(args.jobs)],
                capture_output=True, text=True,
            )  # fmt: skip
            if lockstep.returncode:
                first = next(
                    (line.split(":")[0] for line in lockstep.stdout.splitlines()
                     if ": identical" not in line and not line.startswith(" ")),
                    "?",
                )  # fmt: skip
                caught = f"lockstep ({first})"
        print(f"{name}: {'caught by ' + caught if caught else 'MISSED'}", flush=True)
        missed += caught is None
    print(f"{ran - missed} caught, {missed} missed")
    return 1 if missed else 0


if __name__ == "__main__":
    raise SystemExit(main())
