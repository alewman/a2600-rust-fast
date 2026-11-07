//! A sampling profiler for the board, since `perf` is not allowed here:
//! SIGPROF 1,000 times a second of CPU time, the interrupted instruction's
//! address recorded, then `scripts/profile.py` maps the addresses to source
//! lines (inlined frames included) with `addr2line`.
//!
//!     cargo run --release --features profile --bin a2600-profile -- ROM [--frames N] [--out FILE]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use a2600_rust::cartridge::read_image;
use a2600_rust::System;

const CAPACITY: usize = 1 << 20;
static mut SAMPLES: [usize; CAPACITY] = [0; CAPACITY];
static COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_prof(_: libc::c_int, _: *mut libc::siginfo_t, context: *mut libc::c_void) {
    // SAFETY: the kernel passes a ucontext_t; only the RIP is read, and each
    // slot is written once, by this handler, on this thread.
    unsafe {
        let ucontext = &*(context as *const libc::ucontext_t);
        let rip = ucontext.uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        if n < CAPACITY {
            let samples = &raw mut SAMPLES;
            (*samples)[n] = rip;
        }
    }
}

fn main() {
    let mut frames = 600u64;
    let mut out = "external/profile.txt".to_owned();
    let mut rom = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().expect("N").parse().expect("a number"),
            "--out" => out = args.next().expect("FILE"),
            _ => rom = Some(arg),
        }
    }
    let rom = rom.expect("usage: a2600-profile ROM [--frames N] [--out FILE]");
    let (image, _) = read_image(Path::new(&rom)).unwrap_or_else(|e| panic!("{e}"));
    let mut system = System::ntsc(&image).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: installing a handler and an interval timer for this process.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_prof as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigaction(libc::SIGPROF, &action, std::ptr::null_mut());
        let tick = libc::timeval { tv_sec: 0, tv_usec: 1000 };
        let timer = libc::itimerval { it_interval: tick, it_value: tick };
        libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut());
    }
    for _ in 0..frames {
        system.run_frames(1);
        system.board_mut().tia.audio.take_samples();
    }
    // SAFETY: stop the timer before reading the samples.
    unsafe {
        let zero = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let timer = libc::itimerval { it_interval: zero, it_value: zero };
        libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut());
    }
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let exe = std::fs::read_link("/proc/self/exe").unwrap();
    let base = maps
        .lines()
        .find(|line| line.ends_with(exe.to_str().unwrap()))
        .and_then(|line| usize::from_str_radix(line.split('-').next().unwrap(), 16).ok())
        .expect("the executable's mapping");
    let n = COUNT.load(Ordering::Relaxed).min(CAPACITY);
    let mut text = format!("{}\n", exe.display());
    let samples = &raw const SAMPLES;
    for i in 0..n {
        // SAFETY: the timer is stopped, nothing writes SAMPLES any more.
        let rip = unsafe { (*samples)[i] };
        text.push_str(&format!("{:x}\n", rip.wrapping_sub(base)));
    }
    std::fs::write(&out, text).unwrap();
    eprintln!("{n} samples to {out}");
}
