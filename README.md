# a2600-rust-fast

A speed fork of [a2600-rust](https://github.com/alewman/a2600-rust), the Rust
Atari 2600 board, held in lockstep with it: every change must leave this
board's traces identical to a2600-rust's (and so to a2600-python's at the pin)
on every case, and must measure faster, or it is not kept. It adds the window,
`a2600-play`.

```text
cargo build --release --features frontend
target/release/a2600-play "Pitfall! - Pitfall Harry's Jungle Adventure (USA).zip"
target/release/a2600-play game.bin --machine pal --left paddles --phosphor
```

Arrows and Space are the left joystick, W A S D and Left Shift the right; F1
Select, F2 Reset, F3/F4 colour or B/W, F5-F8 the difficulties; P pause, Tab
fast forward, F11 fullscreen, F12 a screenshot, Esc quit; `--help` lists the
paddles, keypad, driving controller, Booster Grip and SaveKey. Gamepads map
by button position (the first is the left joystick). The window is the
reference's `frontend.py` on SDL2: the board runs faster than real time, so
the sound queue sets the pace (without sound, a timer at the machine's frame
rate does).

## Speed

**x1.886** over the fork point (a2600-rust `e42ec82`), measured directly, on
all five benchmark games alike (x1.85 to x1.94): about 225 frames a second
against 120 on one core of a loaded shared machine, nearly four times real
time.

| Kept | A/B against the build before it |
| --- | ---: |
| HMxx's four-step history as a ring, not a copied array | x1.029 |
| On a quiet step (no object clocked, none in a strobe, hold or deferred step: every falling edge without HMOVE's pulses, and HBLANK's rising edges) skip the object sections, which change only NUSIZ's history | x1.201 |
| The output pins on demand, the colour only on the pixel's falling edge | x1.019 |
| Carry whether a counter is busy rather than scan the five every step | x1.016 |
| Cache the objects' outputs between busy steps and register writes | x1.030 |
| One copy of the step for each clock edge (a const generic) | x1.107 |
| A copy of the object sections for a plain MOTCK step, its inputs as literals | x1.077 |
| The plain step's counter update without the strobe, hold and deferred terms | x1.153 |
| The four merge registers as the bytes of one word | x1.050 |

Measured directly against the fork point after the first eight: x1.796;
after all nine: x1.886.
Tried and not kept, with their numbers: [docs/not-kept.md](docs/not-kept.md).

Where the time goes now: the TIA's half-clock step, still about 85%, with no
single line above 4%; the 6507 and the cartridges together under 5%.

## The rule and the gate

`scripts/ladder.sh` (about 70 seconds): `cargo test`, rung 1 (the TIA against
the 39 die traces, the C27 departures exact), and all 255 lockstep cases
against the reference traces (`external/refcache`, a2600-rust's, identical to
a2600-python's at the pin): the reference's test programs, the smoke group,
the long runs with inputs, and a SaveKey game writing its save. A change that
leaves one trace different is not a speed-up.

**Measuring** on a shared machine: `a2600-bench` times by the thread's own
CPU time (`/proc/thread-self/schedstat`), and `scripts/ab.py` alternates two
builds game by game on one pinned core and compares each side's best of 12
short runs (other work only ever slows a run down; the median moved by 10% on
an A/A run). An A/A run is within 1% overall and 3% a game. **Profiling**:
`perf` is not allowed here, so `a2600-profile` (feature `profile`) samples the
program counter on SIGPROF and `scripts/profile.py` resolves the samples with
`addr2line`, inlined frames included.

**Seeded faults** (`scripts/seeded_faults.py`): a2600-rust's fourteen, and
five in this fork's own shortcuts (the quiet step's NUSIZ history, the pixel
cache over a write, the plain step's counter pending, the plain step taken
under a RESMP lock, the busy flag ignoring the hold): **19 caught, 0
missed**, the fork's five by the die traces alone.

`docs/handoff-brief.md` is a2600-rust's brief; this fork's record is its
history and the tables above.

## Licence

MIT (see `LICENSE`). Nothing third-party is committed.
