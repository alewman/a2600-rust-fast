//! Run a cartridge and write the lockstep trace (`a2600_rust::trace`); the
//! reference's half is `scripts/a2600_trace.py`, same arguments, same text.
//!
//!     a2600-trace ROM [--machine ntsc|pal|secam] [--revision NAME] [--scheme S]
//!                 [--game N] [--slices N] [--steps A:B] [--reset FROM:TO]
//!                 [--select FROM:TO] [--joy PORT:KEYS@FROM:TO] [--paddle I:POS@FROM]
//!                 [--member NAME] [--out FILE]
//!
//! Slices are counted from 0; FROM:TO is half-open. KEYS are any of u d l r
//! f (up, down, left, right, fire).

use std::cell::RefCell;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::rc::Rc;

use a2600_rust::cartridge::{read_image, Options};
use a2600_rust::controllers::{joystick, Paddles, Pads, SaveKey, Stick};
use a2600_rust::machine::{machine, revision};
use a2600_rust::riot::PortA;
use a2600_rust::trace::{crc32, Crc32, HEADER, SLICE_CYCLES};
use a2600_rust::System;

fn range(text: &str) -> (u64, u64) {
    let (a, b) = text.split_once(':').expect("FROM:TO");
    (a.parse().expect("FROM"), b.parse().expect("TO"))
}

/// One named ROM from a zip that holds several (the Stella issue attachments).
fn read_member(path: &std::path::Path, member: &str) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut archive = zip::ZipArchive::new(file).expect("a zip");
    let mut entry = archive
        .by_name(member)
        .unwrap_or_else(|e| panic!("{member}: {e}"));
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut data).expect("read");
    data
}

/// The SaveKey as the RIOT's device, kept so the trace can read its data.
struct Shared(Rc<RefCell<SaveKey>>);

impl PortA for Shared {
    fn lines(&mut self, levels: u8, cycle: i64) -> u8 {
        self.0.borrow_mut().lines(levels, cycle)
    }
}

struct Joy {
    port: usize,
    keys: String,
    from: u64,
    to: u64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut rom: Option<PathBuf> = None;
    let mut machine_name = "ntsc".to_owned();
    let mut revision_name: Option<String> = None;
    let mut scheme: Option<String> = None;
    let mut options = Options::default();
    let mut slices = 150u64;
    let mut steps: Option<(u64, u64)> = None;
    let mut resets = Vec::new();
    let mut selects = Vec::new();
    let mut joys = Vec::new();
    let mut paddles: Vec<(usize, f64, u64)> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut member: Option<String> = None;
    let mut savekey_on = false;
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| panic!("{arg} needs a value"));
        match arg.as_str() {
            "--machine" => machine_name = value(),
            "--revision" => revision_name = Some(value()),
            "--scheme" => scheme = Some(value()),
            "--game" => options.game = Some(value().parse().expect("a game number")),
            "--slices" => slices = value().parse().expect("a number"),
            "--steps" => steps = Some(range(&value())),
            "--reset" => resets.push(range(&value())),
            "--select" => selects.push(range(&value())),
            "--joy" => {
                let text = value();
                let (port, rest) = text.split_once(':').expect("PORT:KEYS@FROM:TO");
                let (keys, span) = rest.split_once('@').expect("PORT:KEYS@FROM:TO");
                let (from, to) = range(span);
                joys.push(Joy {
                    port: port.parse().expect("port"),
                    keys: keys.into(),
                    from,
                    to,
                });
            }
            "--paddle" => {
                let text = value();
                let (pad, rest) = text.split_once(':').expect("I:POS@FROM");
                let (position, from) = rest.split_once('@').expect("I:POS@FROM");
                paddles.push((
                    pad.parse().expect("pad"),
                    position.parse().expect("position"),
                    from.parse().expect("FROM"),
                ));
            }
            "--out" => out = Some(PathBuf::from(value())),
            "--member" => member = Some(value()),
            "--savekey" => savekey_on = true,
            _ if rom.is_none() => rom = Some(PathBuf::from(arg)),
            _ => panic!("unknown argument {arg}"),
        }
    }
    let rom = rom.expect("usage: a2600-trace ROM [options]");
    let image = match &member {
        Some(member) => read_member(&rom, member),
        None => read_image(&rom).unwrap_or_else(|e| panic!("{e}")).0,
    };
    let mut row = machine(&machine_name).unwrap_or_else(|| panic!("no machine {machine_name}"));
    if let Some(name) = &revision_name {
        row.revision = revision(name).unwrap_or_else(|| panic!("no revision {name}"));
    }
    let mut system = System::from_rom(&image, row, scheme.as_deref(), &options)
        .unwrap_or_else(|e| panic!("{e}"));

    let drawn = Rc::new(RefCell::new((Crc32::default(), 0u64)));
    let sink = Rc::clone(&drawn);
    system.board_mut().tia.on_line = Some(Box::new(move |pixels, vsync| {
        let mut drawn = sink.borrow_mut();
        drawn.0.update(pixels);
        drawn.0.update(&[vsync as u8]);
        drawn.1 += 1;
    }));
    let savekey = savekey_on.then(|| {
        let key = Rc::new(RefCell::new(
            SaveKey::new(1, None, row.timing.cpu_hz()).unwrap(),
        ));
        system.board_mut().riot.port_a_device = Some(Box::new(Shared(Rc::clone(&key))));
        key
    });
    if !paddles.is_empty() {
        let hz = row.timing.colour_clock_hz;
        system.board_mut().pads = Pads::Paddles(Paddles::new(hz));
    }
    let ports: Vec<usize> = {
        let mut ports: Vec<usize> = joys.iter().map(|j| j.port).collect();
        ports.sort();
        ports.dedup();
        ports
    };

    let file: Box<dyn Write> = match &out {
        Some(path) => Box::new(std::fs::File::create(path).expect("create the trace")),
        None => Box::new(std::io::stdout().lock()),
    };
    let mut out = BufWriter::new(file);
    writeln!(out, "{HEADER}").unwrap();
    let mut instructions = 0u64;
    let switches = row.switches;
    for k in 0..slices {
        // inputs for this slice
        let held = |spans: &Vec<(u64, u64)>| spans.iter().any(|&(a, b)| a <= k && k < b);
        let mut input_b = switches;
        if held(&resets) {
            input_b &= !0x01;
        }
        if held(&selects) {
            input_b &= !0x02;
        }
        let board = system.board_mut();
        board.riot.input_b = input_b;
        for &port in &ports {
            let mut keys = String::new();
            for j in joys
                .iter()
                .filter(|j| j.port == port && j.from <= k && k < j.to)
            {
                keys.push_str(&j.keys);
            }
            let has = |c| keys.contains(c);
            let stick = Stick {
                up: has('u'),
                down: has('d'),
                left: has('l'),
                right: has('r'),
                fire: has('f'),
            };
            joystick(&mut board.riot, &mut board.tia, port, stick);
        }
        if let Pads::Paddles(p) = &mut board.pads {
            for &(pad, position, from) in &paddles {
                if from <= k {
                    p.position[pad] = Some(position);
                }
            }
        }

        let end = (k as i64 + 1) * SLICE_CYCLES;
        while system.cpu.bus.cycles < end {
            if let Some((a, b)) = steps {
                if a <= instructions && instructions < b {
                    let cpu = &system.cpu;
                    writeln!(
                        out,
                        "I {instructions} c={} cpu={:02X}{:02X}{:02X}{:02X}{:04X}{:02X}",
                        cpu.bus.cycles, cpu.a, cpu.x, cpu.y, cpu.s, cpu.pc, cpu.p
                    )
                    .unwrap();
                }
            }
            system.cpu.step();
            instructions += 1;
        }
        let samples = system.board_mut().tia.audio.take_samples();
        let mut audio = Crc32::default();
        for sample in &samples {
            audio.update(&sample.to_le_bytes());
        }
        let (lines_crc, lines) = {
            let mut drawn = drawn.borrow_mut();
            let result = (drawn.0.value(), drawn.1);
            *drawn = (Crc32::default(), 0);
            result
        };
        let cpu = &system.cpu;
        let board = &cpu.bus;
        let (riot, tia) = (&board.riot, &board.tia);
        write!(
            out,
            "S {k} c={} i={instructions} h={} cpu={:02X}{:02X}{:02X}{:02X}{:04X}{:02X} ram={:08X} \
             timer={:02X}/{:02X}/{}/{}/{}/{} px={:08X}/{} au={:08X}/{} tia={}/{}/{}/{}/{:04X}/{}{:02X} bus={:02X}",
            board.cycles,
            board.halted_cycles,
            cpu.a, cpu.x, cpu.y, cpu.s, cpu.pc, cpu.p,
            crc32(&riot.ram),
            riot.value, riot.flags, riot.divider, riot.countdown, riot.as_of, riot.wrapped_at,
            lines_crc, lines,
            audio.value(), samples.len(),
            tia.frame, tia.line, tia.step_in_line, tia.hcount, tia.collisions, tia.vsync as u8, tia.vblank,
            board.data_bus,
        )
        .unwrap();
        if let Some(key) = &savekey {
            let key = key.borrow();
            write!(out, " sk={:08X}/{}", crc32(&key.data), key.pointer).unwrap();
        }
        writeln!(out).unwrap();
    }
}
