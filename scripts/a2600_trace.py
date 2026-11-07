#!/usr/bin/env python3
"""Run a cartridge on a2600-python and write the lockstep trace (the reference's half).

The same arguments as ``a2600-trace`` and the same text, line for line, as
``a2600_rust::trace`` documents. Run it with the reference's interpreter
(``external/a2600-python/.venv-pypy/bin/python``, from
``scripts/fetch_a2600python.sh``).
"""

from __future__ import annotations

import argparse
import struct
import sys
import zipfile
import zlib
from dataclasses import replace
from pathlib import Path

from a2600_python import MACHINES, System
from a2600_python.cartridge import read_image
from a2600_python.controllers import Joystick, Paddles, SaveKey
from a2600_python.machine import REVISIONS

HEADER = "# a2600-trace 1"
SLICE_CYCLES = 19_912


def span(text: str) -> tuple[int, int]:
    a, b = text.split(":")
    return int(a), int(b)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("rom", type=Path)
    parser.add_argument("--machine", default="ntsc")
    parser.add_argument("--revision")
    parser.add_argument("--scheme")
    parser.add_argument("--game", type=int)
    parser.add_argument("--slices", type=int, default=150)
    parser.add_argument("--steps", type=span)
    parser.add_argument("--reset", type=span, action="append", default=[])
    parser.add_argument("--select", type=span, action="append", default=[])
    parser.add_argument("--joy", action="append", default=[])
    parser.add_argument("--paddle", action="append", default=[])
    parser.add_argument("--member", help="the ROM inside a zip that holds several")
    parser.add_argument("--savekey", action="store_true", help="a blank SaveKey on the right port")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    if args.member:
        with zipfile.ZipFile(args.rom) as archive:
            image = archive.read(args.member)
    else:
        image = read_image(args.rom)[0]
    row = MACHINES[f"Atari VCS ({args.machine.upper()})"]
    if args.revision:
        row = replace(row, revision=REVISIONS[args.revision])
    options = {}
    if args.scheme:
        options["scheme"] = args.scheme
    if args.game is not None:
        options["game"] = args.game
    system = System.from_rom(image, row, **options)
    board = system.board
    tia, riot = board.tia, board.riot

    drawn = [zlib.crc32(b""), 0]

    def on_line(pixels: bytearray) -> None:
        drawn[0] = zlib.crc32(bytes(pixels) + bytes([tia.vsync]), drawn[0])
        drawn[1] += 1

    tia.on_line = on_line
    joys = []
    for text in args.joy:
        port, rest = text.split(":", 1)
        keys, when = rest.split("@")
        joys.append((int(port), keys, *span(when)))
    pads = []
    for text in args.paddle:
        pad, rest = text.split(":", 1)
        position, when = rest.split("@")
        pads.append((int(pad), float(position), int(when)))
    paddles = Paddles(tia, row.timing.colour_clock_hz) if pads else None
    savekey = SaveKey(riot, 1, None, row.timing.cpu_hz) if args.savekey else None
    ports = sorted({j[0] for j in joys})
    sticks = {port: Joystick(riot, tia, port) for port in ports}

    out = args.out.open("w") if args.out else sys.stdout
    out.write(HEADER + "\n")
    instructions = 0
    step = system.cpu.step
    cpu = system.cpu
    for k in range(args.slices):
        input_b = row.switches
        if any(a <= k < b for a, b in args.reset):
            input_b &= ~0x01
        if any(a <= k < b for a, b in args.select):
            input_b &= ~0x02
        riot.input_b = input_b
        for port in ports:
            keys = "".join(j[1] for j in joys if j[0] == port and j[2] <= k < j[3])
            sticks[port].set(
                up="u" in keys, down="d" in keys, left="l" in keys, right="r" in keys,
                fire="f" in keys,
            )  # fmt: skip
        if paddles is not None:
            for pad, position, when in pads:
                if when <= k:
                    paddles.position[pad] = position

        end = (k + 1) * SLICE_CYCLES
        steps = args.steps
        while board.cycles < end:
            if steps is not None and steps[0] <= instructions < steps[1]:
                out.write(
                    f"I {instructions} c={board.cycles} cpu={cpu.a:02X}{cpu.x:02X}{cpu.y:02X}"
                    f"{cpu.s:02X}{cpu.pc:04X}{cpu.p:02X}\n"
                )
            step()
            instructions += 1
        samples = tia.audio.take_samples()
        audio = zlib.crc32(b"".join(struct.pack("<d", s) for s in samples))
        lines_crc, lines = drawn
        drawn[0], drawn[1] = zlib.crc32(b""), 0
        out.write(
            f"S {k} c={board.cycles} i={instructions} h={board.halted_cycles} "
            f"cpu={cpu.a:02X}{cpu.x:02X}{cpu.y:02X}{cpu.s:02X}{cpu.pc:04X}{cpu.p:02X} "
            f"ram={zlib.crc32(bytes(riot.ram)):08X} "
            f"timer={riot.value:02X}/{riot.flags:02X}/{riot.divider}/{riot._countdown}/"
            f"{riot._as_of}/{riot._wrapped_at} "
            f"px={lines_crc:08X}/{lines} au={audio:08X}/{len(samples)} "
            f"tia={tia.frame}/{tia.line}/{tia.step_in_line}/{tia.hcount}/{tia.collisions:04X}/"
            f"{int(tia.vsync)}{tia.vblank:02X} bus={board.data_bus:02X}"
            + (f" sk={zlib.crc32(bytes(savekey.data)):08X}/{savekey.pointer}" if savekey else "")
            + "\n"
        )
    if args.out:
        out.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
