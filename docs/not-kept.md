# Tried and not kept

Each with its A/B against the build before it (scripts/ab.py, best of 12
alternating runs a game, five games; A/A noise about 1% overall, 3% a game).

- **HMxx's history as "the value before the last change, and its age"**
  instead of the four-entry ring (the comparators see HMxx four steps late;
  writes are at least six steps apart, so one remembered change would do):
  x1.004, noise, and it trades a ring for a timing argument. Not kept.
- **Fat LTO, one codegen unit** (`lto = "fat"`, `codegen-units = 1`):
  x0.998. The hot code is all in this crate already. Not kept.
- **HMxx and its history as packed words** (five nibbles a step, the
  four-step history one `u128`): x1.009, within the noise, and a second
  copy of HMxx to keep in step with the first. Not kept.
- **The board calling each edge's copy directly** (every cycle is six steps
  from a rising edge, so `rise()`/`fall()` in fixed order, no edge test):
  x1.010, at the noise; the assumption held under debug assertions. Not kept.
