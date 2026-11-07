#!/usr/bin/env python3
"""Write scripts/cases.tsv, the lockstep gate's cases (names and arguments, no ROMs).

    make_cases.py

Rung 2 is the reference's own test programs: every ROM in the Stella issue
attachments ($TEST_ROMS), the #699 serial ROMs on the PAL row under each
console's revision with RESET held for four slices, as scripts/serial_699.py
does ($HARDWARE), and the die cartridges ($DIE_CARTS). Rung 3 is the smoke
group (tests/smoke_dumps.txt at the pin: every scheme, PAL, the residue and
the classics), 150 slices each. Rung 4 is long runs with inputs. Paths are
resolved here once, against the defaults scripts/lockstep.py uses.
"""

from __future__ import annotations

import shlex
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REF = ROOT / "external" / "a2600-python"
DEFAULTS = {
    "ROMS": "/data/emu/roms/atari2600",
    "RETRO": "/data/emu/roms-retrobat/atari2600",
    "TEST_ROMS": "/data/emu/a2600-python/tests/test_roms/stella-issues",
    "HARDWARE": "/data/emu/a2600-python/reference/hardware/stella-issues",
    "DIE_CARTS": str(REF / "tests" / "die_carts"),
}
REVISIONS = [
    "10444D (reference)", "PAL 7800 (#699)", "SECAM woody (#699)", "PAL Jr. '84 (#699)",
    "PAL Jr. '91 (#699)", "PAL Vader (#699)", "PAL light sixer (#699)",
]  # fmt: skip

# Long runs: (name, slices, dump, arguments).
LONG = [
    ("long-pitfall", 3000, "Pitfall! - Pitfall Harry's Jungle Adventure (USA)",
     "--reset 30:60 --joy 0:r@100:400 --joy 0:rf@400:410 --joy 0:l@600:900 --joy 0:d@1000:1100 --joy 0:r@1200:2400"),
    ("long-river-raid", 2000, "River Raid (USA)",
     "--reset 30:60 --joy 0:uf@100:700 --joy 0:lf@700:800 --joy 0:rf@800:900 --joy 0:u@900:1900"),
    ("long-adventure", 2000, "Adventure (USA)",
     "--select 20:30 --reset 40:70 --joy 0:u@100:300 --joy 0:r@300:600 --joy 0:d@600:800 --joy 0:f@800:820"),
    ("long-kaboom", 1500, "Kaboom! (USA)",
     "--paddle 0:0.5@0 --reset 30:60 --paddle 0:0.2@200 --paddle 0:0.8@400 --paddle 0:0.35@700 --paddle 0:0.6@1000"),
    ("long-space-invaders", 2000, "Space Invaders (USA)",
     "--reset 30:60 --joy 0:f@100:1900 --joy 0:r@300:500 --joy 0:l@700:900"),
    ("long-pitfall-ii", 2000, "David Crane's Pitfall II - Lost Caverns (USA)",
     "--reset 30:60 --joy 0:r@100:500 --joy 0:rf@500:510 --joy 0:r@600:1500"),
    ("long-combat-pal", 1000, "Combat (Europe)", "--machine pal --reset 30:60 --joy 0:u@100:500 --joy 1:uf@100:600"),
    ("long-burgertime", 1500, "BurgerTime (USA)", "--reset 30:60 --joy 0:r@100:400 --joy 0:l@400:700"),
    ("long-decathlon", 1500, "Activision Decathlon, The (USA)", "--reset 30:60 --joy 0:lr@100:900"),
    ("long-dragonstomper", 1500, "Dragonstomper (USA)", "--reset 30:60 --joy 0:f@100:110 --joy 0:r@200:600"),
    # A SaveKey game: it writes its save and reads it back through the EEPROM.
    ("long-savekey-go-fish", 600, "Go Fish! (World) (NTSC) (Aftermarket) (Unl)",
     "--savekey --reset 30:60 --joy 0:f@100:110 --joy 0:r@200:260 --joy 0:f@300:310 --reset 400:420"),
]  # fmt: skip


def dump(name: str) -> str:
    if (Path(DEFAULTS["ROMS"]) / f"{name}.zip").is_file():
        return f"$ROMS/{name}.zip"
    if (Path(DEFAULTS["RETRO"]) / f"{name}.7z").is_file():
        return f"$RETRO/{name}.7z"
    raise SystemExit(f"no local dump named {name!r}")


def main() -> int:
    rows = []
    # Rung 2: the reference's test programs.
    for archive in sorted(Path(DEFAULTS["TEST_ROMS"]).iterdir()):
        if archive.suffix.lower() != ".zip":
            continue
        with zipfile.ZipFile(archive) as z:
            members = [
                n for n in z.namelist()
                if n.lower().endswith((".bin", ".a26", ".rom")) and "__MACOSX" not in n
            ]  # fmt: skip
        for member in members:
            stem = Path(member).stem.replace(" ", "_")
            name = f"test-{archive.stem}-{stem}"
            machine = "--machine pal " if "PAL" in member or archive.name.startswith("699") else ""
            if archive.name.startswith("699"):
                for revision in REVISIONS:
                    short = revision.split(" (")[0].replace(" ", "_").replace("'", "")
                    rows.append((f"{name}-{short}", 200, f"$TEST_ROMS/{archive.name}",
                                 f"--member {shlex.quote(member)} {machine}--revision {shlex.quote(revision)} --reset 0:4"))
            else:
                rows.append((name, 60, f"$TEST_ROMS/{archive.name}", f"--member {shlex.quote(member)} {machine}".strip()))
    for archive, member in (("699-HMOVE_TEST.zip", "HMOVE_TEST/early_HMOVE.bin"),
                            ("699-HMOVE_RESPx_test.zip", "HMOVE_RESPx_test/HMOVE_RESPx.bin")):
        for revision in REVISIONS:
            short = revision.split(" (")[0].replace(" ", "_").replace("'", "")
            rows.append((f"hw-{Path(member).stem}-{short}", 200, f"$HARDWARE/{archive}",
                         f"--member {shlex.quote(member)} --machine pal --revision {shlex.quote(revision)} --reset 0:4"))
    die_list = (REF / "scripts" / "die_carts.py").read_text()
    for line in die_list.splitlines():
        line = line.strip()
        if line.startswith("- ``") and "``:" in line:
            cart = line.split("``")[1]
            rows.append((f"die-{cart}", 20, f"$DIE_CARTS/{cart}.bin", ""))
    # Rung 3: the smoke group.
    smoke = (REF / "tests" / "smoke_dumps.txt").read_text().splitlines()
    for line in smoke:
        name = line.partition("#")[0].strip()
        if name:
            args = "--machine pal" if "(Europe" in name or "PAL" in name else ""
            rows.append((f"smoke-{name}", 150, dump(name), args))
    # Rung 4: long runs.
    for case, slices, name, args in LONG:
        rows.append((case, slices, dump(name), args))
    names = [r[0] for r in rows]
    duplicates = sorted({n for n in names if names.count(n) > 1})
    if duplicates:
        raise SystemExit(f"case names must be unique: {duplicates}")
    lines = ["# name\tslices\trom\targuments (shell words); $ROMS $RETRO $TEST_ROMS $HARDWARE $DIE_CARTS"]
    lines += [f"{n}\t{s}\t{r}\t{a}" for n, s, r, a in rows]
    (ROOT / "scripts" / "cases.tsv").write_text("\n".join(lines) + "\n")
    print(f"{len(rows)} cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
