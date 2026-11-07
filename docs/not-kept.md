# Tried and not kept

Each with its A/B against the build before it (scripts/ab.py, best of 12
alternating runs a game, five games; A/A noise about 1% overall, 3% a game).

- **HMxx's history as "the value before the last change, and its age"**
  instead of the four-entry ring (the comparators see HMxx four steps late;
  writes are at least six steps apart, so one remembered change would do):
  x1.004, noise, and it trades a ring for a timing argument. Not kept.
- **Fat LTO, one codegen unit** (`lto = "fat"`, `codegen-units = 1`):
  x0.998. The hot code is all in this crate already. Not kept.
