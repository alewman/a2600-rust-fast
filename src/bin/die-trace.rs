//! Rung 1: replay Sim2600 die traces into the TIA and score every pin, every
//! half clock (the reference's `scripts/die_trace.py` and
//! `tests/test_die_traces.py`).
//!
//!     die-trace TRACE [...] [--warm N] [--report N]
//!
//! The traces record, for each half colour clock, what the console drove into
//! the TIA's pins and what the simulated 10444D drove back; this replays the
//! inputs into the TIA and compares its pins with the die's: PH0, RDY, SYNC,
//! luminance, colour, blanking, the data-bus drivers and both audio pads.
//!
//! **Writes** are decoded from the pins: a write lands on the record where
//! CLK2 falls with the TIA selected and R/W low. With the console's wiring
//! the record in which CLK2 falls already shows the next cycle's bus, while
//! the die latched the old one, so the bus is held while CLK2 is low. The
//! wiring is detected from how CLK2 follows PH0.
//!
//! **Scoring** starts at the first HBLANK rise after record 2,000. A trace
//! passes when it matches everywhere, or where the board follows seven
//! consoles over the die (C27) by exactly the reference's counted half clocks.

use a2600_rust::tia::Tia;
use std::path::Path;

const PINS: [&str; 9] = [
    "ph0", "rdy", "sync", "lum", "col", "blank", "dbdrv", "au0", "au1",
];

/// The reference's `KNOWN_DEPARTURES`: hmove_cycles' two HMOVEs on cycle 74,
/// where seven consoles set the comb latch and Sim2600 clears it (C27), and
/// the same tie in hmove_resp0's HMOVE-on-cycle-74 block.
fn known(name: &str) -> [u64; 9] {
    match name {
        "hmove_cycles.trace" => [0, 0, 0, 230, 220, 32, 0, 0, 0],
        "hmove_resp0.trace" => [0, 0, 0, 2638, 2638, 352, 0, 0, 0],
        _ => [0; 9],
    }
}

fn load(path: &Path) -> Vec<[u8; 17]> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut records = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut record = [0u8; 17];
        for (slot, field) in record.iter_mut().zip(line.split_whitespace()) {
            *slot = u8::from_str_radix(field, 16).expect("hex field");
        }
        records.push(record);
    }
    records
}

/// With the console's wiring CLK2 falls one record after PH0 falls.
fn console_wiring(records: &[[u8; 17]]) -> bool {
    let end = records.len().min(20000);
    let votes = (1..end)
        .filter(|&k| records[k - 1][1] == 1 && records[k][1] == 0 && records[k - 1][8] == 0)
        .count();
    let falls = (1..end)
        .filter(|&k| records[k - 1][1] != 0 && records[k][1] == 0)
        .count();
    votes > falls / 2
}

fn replay(records: &[[u8; 17]], warm: usize, report: usize) -> (u64, [u64; 9], bool) {
    let console = console_wiring(records);
    let mut tia = Tia::new(0);
    tia.rising = records[0][0] == 1;
    let start = (warm.max(1)..records.len())
        .find(|&k| records[k][13] != 0 && records[k - 1][13] == 0)
        .unwrap_or(records.len());
    let mut mismatches = [0u64; 9];
    let mut shown = 0;
    let mut bus = records[0];
    let mut scored = 0u64;
    for (k, rec) in records.iter().enumerate() {
        let (clk0, clk2) = (rec[0], rec[1]);
        if !console || clk2 != 0 {
            bus = *rec;
        }
        if tia.rising != (clk0 == 1) {
            panic!("record {k}: CLK0 does not alternate");
        }
        let prev_clk2 = if k > 0 { records[k - 1][1] } else { 0 };
        let selected = bus[3] == 0 && bus[4] == 0;
        let write = if prev_clk2 != 0 && clk2 == 0 && selected && bus[2] == 0 {
            Some((bus[5], bus[6]))
        } else {
            None
        };
        tia.inputs = rec[7];
        tia.step(write);
        if k < start {
            continue;
        }
        scored += 1;
        let mut dbdrv = 0;
        if selected && bus[2] == 1 && clk2 != 0 {
            let d = tia.read(bus[5]);
            dbdrv = (if d & 0x80 != 0 { 8 } else { 4 }) | (if d & 0x40 != 0 { 2 } else { 1 });
        }
        let pins = tia.pins();
        let ours = [
            tia.ph0 as u8,
            tia.rdy_low as u8,
            pins.sync_low as u8,
            pins.lum,
            pins.col,
            pins.blank as u8,
            dbdrv,
            pins.au0,
            pins.au1,
        ];
        let theirs = [
            rec[8], rec[9], rec[10], rec[11], rec[12], rec[13], rec[16], rec[14], rec[15],
        ];
        let mut bad = Vec::new();
        for p in 0..9 {
            if ours[p] != theirs[p] {
                mismatches[p] += 1;
                bad.push(format!("{} {}/{}", PINS[p], ours[p], theirs[p]));
            }
        }
        if !bad.is_empty() && shown < report {
            println!(
                "  record {k} (line step {}): {}   (ours/die)",
                tia.step_in_line,
                bad.join(", ")
            );
            shown += 1;
        }
    }
    (scored, mismatches, console)
}

fn main() {
    let mut paths = Vec::new();
    let (mut warm, mut report) = (2000usize, 0usize);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--warm" => warm = args.next().expect("--warm N").parse().expect("a number"),
            "--report" => report = args.next().expect("--report N").parse().expect("a number"),
            _ => paths.push(arg),
        }
    }
    let mut failed = 0;
    for path in &paths {
        let path = Path::new(path);
        let records = load(path);
        let (scored, mismatches, console) = replay(&records, warm, report);
        let name = path.file_name().unwrap().to_string_lossy();
        let expected = known(&name);
        let total: u64 = mismatches.iter().sum();
        let summary: Vec<String> = (0..9)
            .filter(|&p| mismatches[p] != 0)
            .map(|p| format!("{} {}", PINS[p], mismatches[p]))
            .collect();
        let verdict = if total == 0 {
            "MATCH"
        } else if mismatches == expected {
            "MATCH (the counted C27 departures)"
        } else {
            failed += 1;
            "MISMATCH"
        };
        println!(
            "{name}: {verdict}: {scored} records scored, {} wiring{}",
            if console { "console" } else { "Sim2600" },
            if summary.is_empty() {
                String::new()
            } else {
                format!("; {}", summary.join(", "))
            }
        );
    }
    println!("{} of {} traces pass", paths.len() - failed, paths.len());
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
