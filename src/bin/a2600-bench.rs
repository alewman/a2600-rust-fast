//! The benchmark: run cartridges headless for N frames each and print the
//! frames per second, with a checksum of every line drawn and every audio
//! sample, so a faster build that changes the output is caught here too.
//!
//!     a2600-bench ROM [...] [--frames N]
//!
//! The time is this thread's time on the CPU (`/proc/thread-self/schedstat`,
//! nanoseconds run), not the wall clock: on a shared machine the wall clock
//! counts the time other processes had the CPU.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Nanoseconds this thread has run on a CPU.
fn cpu_ns() -> u64 {
    let text = std::fs::read_to_string("/proc/thread-self/schedstat").expect("schedstat");
    text.split_whitespace().next().unwrap().parse().unwrap()
}

use a2600_rust::cartridge::read_image;
use a2600_rust::trace::Crc32;
use a2600_rust::System;

fn main() {
    let mut frames = 600u64;
    let mut roms = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--frames" {
            frames = args.next().expect("--frames N").parse().expect("a number");
        } else {
            roms.push(arg);
        }
    }
    for rom in &roms {
        let (image, _) = read_image(Path::new(rom)).unwrap_or_else(|e| panic!("{e}"));
        let mut system = System::ntsc(&image).unwrap_or_else(|e| panic!("{e}"));
        let check = Rc::new(RefCell::new(Crc32::default()));
        let sink = Rc::clone(&check);
        system.board_mut().tia.on_line = Some(Box::new(move |pixels, vsync| {
            let mut crc = sink.borrow_mut();
            crc.update(pixels);
            crc.update(&[vsync as u8]);
        }));
        let began = cpu_ns();
        for _ in 0..frames {
            system.run_frames(1);
            for sample in system.board_mut().tia.audio.take_samples() {
                check.borrow_mut().update(&sample.to_le_bytes());
            }
        }
        let seconds = (cpu_ns() - began) as f64 / 1e9;
        let name = Path::new(rom).file_stem().unwrap().to_string_lossy();
        println!(
            "{name}: {frames} frames in {seconds:.3} s, {:.1} frames/s, check {:08X}",
            frames as f64 / seconds,
            check.borrow().value()
        );
    }
}
