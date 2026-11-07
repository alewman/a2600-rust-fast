//! The TIA, half colour clock by half colour clock: picture, sync, RDY,
//! collisions, inputs (`tia.py`).
//!
//! Modelled as the circuit (the reference's docs/timing.md), and judged
//! against the die: the visual6502 netlist of the 10444D simulated by
//! Sim2600, through tia-fpga's recorded traces (every pin, every half clock).
//! The timing rules are those Towers read off the schematics as corrected by
//! the netlist findings tia-fpga published (cited below as *tia-fpga*). The
//! sound channels are in `audio`.
//!
//! **Time.** One step is one half colour clock: a rising or a falling edge of
//! the colour clock. A line is 456 steps; a CPU cycle is six. Each step
//! computes the next state of every latch from the present state, then
//! commits it all at once, and the pins are read from the committed state, as
//! synchronous logic does.
//!
//! **Writes** land on the step where the 6507's Phi2 falls (tia-fpga "The
//! bus"). A register takes the value on that step and shows it from the next;
//! a strobe (WSYNC, RSYNC, RESxx) also has a window of three steps, the three
//! half clocks CLK2 is low after the write.
//!
//! **Counters** are 6-bit polynomial counters (Towers "Polynomial Counters"),
//! kept here as the count they represent (0-56 for the horizontal counter,
//! 0-39 for objects): a bijection with the LFSR's states. The one state
//! outside it, the illegal all-ones the counters can power up in and reset
//! from, is [`LFSR_ERROR`].
//!
//! Differences forced by Rust: the reference's `on_line` callable is a boxed
//! closure here, given the line and the VSYNC level the television reads at
//! that moment; its `pad_source` callable is the `pads` argument of
//! [`Tia::read_with`], which the board fills from the controllers it owns.

// The loops over P0 P1 M0 M1 BL index several parallel arrays at once, as
// the reference's do; iterators over one of them would hide that.
#![allow(clippy::needless_range_loop)]

use crate::audio::Audio;

pub const LINE_STEPS: u32 = 456; // half colour clocks per line
pub const LFSR_ERROR: i32 = -1;

// Horizontal counter decodes (Towers' table, lines 131-206).
const SHS: i32 = 4;
const RHS: i32 = 8;
const RHB: i32 = 16;
const LRHB: i32 = 18;
const CNT: i32 = 36;
const SHB: i32 = 56;
// Object counter START decodes (Towers 252-293).
const CLOSE: i32 = 3;
const MEDIUM: i32 = 7;
const FAR: i32 = 15;
const MAIN: i32 = 39;

// Write addresses (A0-A5).
pub const VSYNC: i32 = 0x00;
pub const VBLANK: i32 = 0x01;
pub const WSYNC: i32 = 0x02;
pub const RSYNC: i32 = 0x03;
pub const NUSIZ0: i32 = 0x04;
pub const NUSIZ1: i32 = 0x05;
pub const COLUP0: i32 = 0x06;
pub const COLUBK: i32 = 0x09;
pub const CTRLPF: i32 = 0x0A;
pub const REFP0: i32 = 0x0B;
pub const REFP1: i32 = 0x0C;
pub const PF0: i32 = 0x0D;
pub const PF2: i32 = 0x0F;
pub const RESP0: i32 = 0x10;
pub const AUDC0: i32 = 0x15;
pub const AUDV1: i32 = 0x1A;
pub const GRP0: i32 = 0x1B;
pub const GRP1: i32 = 0x1C;
pub const ENAM0: i32 = 0x1D;
pub const ENAM1: i32 = 0x1E;
pub const ENABL: i32 = 0x1F;
pub const HMP0: i32 = 0x20;
pub const HMBL: i32 = 0x24;
pub const VDELP0: i32 = 0x25;
pub const VDELP1: i32 = 0x26;
pub const VDELBL: i32 = 0x27;
pub const RESMP0: i32 = 0x28;
pub const RESMP1: i32 = 0x29;
pub const HMOVE: i32 = 0x2A;
pub const HMCLR: i32 = 0x2B;
pub const CXCLR: i32 = 0x2C;

// Audio ticks: phase A and phase B at these horizontal counts (tia-fpga "Audio").
const AUDIO_A: [i32; 2] = [1, 19];
const AUDIO_B: [i32; 2] = [9, 37];

/// NUSIZ D2-D0: which decodes besides the main one start a copy (Towers 252-293).
fn copies(nusiz: u8, count: i32) -> bool {
    match nusiz {
        0b001 => count == CLOSE,
        0b010 => count == MEDIUM,
        0b011 => count == CLOSE || count == MEDIUM,
        0b100 => count == FAR,
        0b110 => count == MEDIUM || count == FAR,
        _ => false,
    }
}

/// Playfield bits, left to right within a half line: PF0 D4-D7, PF1 D7-D0,
/// PF2 D0-D7 (Stella Programmer's Guide), as (register index, bit).
const PF_BITS: [(usize, u8); 20] = [
    (0, 4),
    (0, 5),
    (0, 6),
    (0, 7),
    (1, 7),
    (1, 6),
    (1, 5),
    (1, 4),
    (1, 3),
    (1, 2),
    (1, 1),
    (1, 0),
    (2, 0),
    (2, 1),
    (2, 2),
    (2, 3),
    (2, 4),
    (2, 5),
    (2, 6),
    (2, 7),
];

/// Collision latch bits, and which two each read address returns in D7, D6.
const COLLISION_READS: [(u32, Option<u32>); 8] = [
    (0, Some(1)),
    (2, Some(3)),
    (4, Some(5)),
    (6, Some(7)),
    (8, Some(9)),
    (10, Some(11)),
    (12, None),
    (13, Some(14)),
];

fn next_count(count: i32, period: i32) -> i32 {
    if 0 <= count && count < period - 1 {
        count + 1
    } else {
        0
    }
}

/// A movable object's position counter and its two-phase clock (Towers 230-251).
///
/// The two-phase clock is a ring of four stages stepped by the object's
/// clock: stage 0 begins H@1, 1 ends it (START decodes are latched there), 2
/// is H@2, where the counter steps. A RESxx strobe does not zero the counter:
/// it holds the ring in H@1 for as long as its window lasts and sets a latch,
/// and the next H@2 clears the counter (tia-fpga "Objects"). The window
/// reaches the ring three steps after the strobe's own three steps.
#[derive(Clone, Debug, Default)]
pub struct ObjectCounter {
    pub count: i32,
    pub ring: u8,
    pub pending: bool,
    /// decided as H@1 ends, applied at H@2
    pub clear_pending: bool,
    /// a stuffed pulse caught by a RESxx, due after it
    pub deferred: bool,
    /// steps of the RESxx strobe still to come
    pub strobe_left: u8,
    /// 3 bits: the strobe window, delayed
    pub window_history: u8,
}

/// START latches, the stretch gate and the 3-bit scan counter (Towers 356-411).
#[derive(Clone, Debug, Default)]
pub struct Player {
    pub start_latched: bool,
    pub start_q: bool,
    pub start_q2: bool,
    /// FSTOB: drawing a close/medium/far copy, not the main one
    pub copy_flag: bool,
    /// NUSIZ D2-D0, one and two steps ago
    pub size_history: (u8, u8),
    pub gate: bool,
    pub scan: u8,
    pub scanning: bool,
}

/// The START count and the count after it, for missile and ball widths (Towers 943-968).
#[derive(Clone, Debug, Default)]
pub struct Width {
    pub start_latched: bool,
    pub first: bool,
    pub second: bool,
}

/// 1 and 2 pixels are the first colour clocks of the START count, 4 the
/// count, 8 the count and the one after (tia-fpga "Objects").
fn width_active(width: &Width, size: u8, ring: u8) -> bool {
    match size {
        0 => width.first && ring == 3,
        1 => width.first && (ring == 3 || ring == 0),
        2 => width.first,
        _ => width.first || width.second,
    }
}

/// A line the TIA has finished: its 228 colour clocks and VSYNC as it then stands.
/// The TIA's output pins as the die-trace replay scores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pins {
    pub au0: u8,
    pub au1: u8,
    pub sync_low: bool,
    pub blank: bool,
    pub col: u8,
    pub lum: u8,
}

pub type LineSink = Box<dyn FnMut(&[u8; 228], bool)>;

pub struct Tia {
    pub line: u64,
    pub frame: u64,
    /// The picture: one byte per colour clock of the line being drawn, the
    /// colour (D7-D1, as COLUxx) with D0 set when not blanked, taken on each
    /// falling edge; `on_line` receives each finished line.
    pub pixels: [u8; 228],
    pub on_line: Option<LineSink>,
    pub registers: [u8; 0x40],
    pub audio: Audio,
    // Write registers.
    pub vsync: bool,
    pub vblank: u8,
    pub nusiz: [u8; 2],
    pub missile_size: [u8; 2],
    /// COLUP0, COLUP1, COLUPF, COLUBK
    pub colours: [u8; 4],
    pub ctrlpf: u8,
    pub reflect: [bool; 2],
    pub pf: [u8; 3],
    pub grp_new: [u8; 2],
    pub grp_old: [u8; 2],
    pub enam: [bool; 2],
    pub enabl_new: bool,
    pub enabl_old: bool,
    /// P0 P1 M0 M1 BL: HMxx D7-D4
    pub hm: [u8; 5],
    pub vdelp: [bool; 2],
    pub vdelbl: bool,
    pub resmp: [bool; 2],
    // Horizontal counter.
    pub hcount: i32,
    /// falling edges since the last H@2, mod 4
    pub hphase: u8,
    pub hblank: bool,
    pub hsync: bool,
    pub rsync_window: u8,
    pub rsync_wait: u8,
    /// PH0 divider, counted on both edges; high for 3, 4, 5
    pub cpu_divider: u8,
    // RDY.
    pub rdy_low: bool,
    pub release_left: u8,
    pub wsync_left: u8,
    // HMOVE.
    pub hmove_latch: bool,
    /// machine::Revision as (objects whose stuffed pulse survives the RESxx
    /// strobe itself (C28), objects a merged pulse steps before it absorbs
    /// their next MOTCK)
    pub revision: (u8, u8),
    pub motion_count: i32,
    /// P0 P1 M0 M1 BL as bits 4..0
    pub motion_more: u8,
    pub motion_armed: bool,
    pub motion_running: bool,
    /// 12 steps of H@1, H@2 and HMOVE strobes
    pub h1_history: u16,
    pub h2_history: u16,
    pub hmove_history: u16,
    /// HMxx 1 to 4 steps ago
    /// HMxx as committed on each of the last 4 steps, a ring: `hm_pos` is
    /// the slot the next commit writes, which holds HMxx from 4 steps ago
    pub hm_late: [[u8; 5]; 4],
    pub hm_pos: usize,
    /// merged pulses, steps ago
    pub merge_1: u8,
    pub merge_2: u8,
    pub merge_3: u8,
    pub merge_4: u8,
    pub merged: u8,
    /// Some object counter is in a RESxx strobe, its hold, or a deferred
    /// step: set at the end of the object sections, which alone change those
    pub counters_busy: bool,
    /// The objects' outputs as `object_pixels` last gave them, while nothing
    /// they depend on has changed: only the object sections (on a busy step)
    /// and register writes change it
    pixels_cache: Option<[bool; 5]>,
    // Objects: P0 P1 M0 M1 BL.
    pub counters: [ObjectCounter; 5],
    pub players: [Player; 2],
    pub missiles: [Width; 2],
    pub ball: Width,
    // Playfield.
    pub pf_index: i32,
    pub pf_down: bool,
    pub pf_read: bool,
    pub pf_shown: bool,
    /// Collision latches, bit order as COLLISION_READS.
    pub collisions: u16,
    // Output latches.
    pub out_objects: [bool; 5],
    pub out_colours: [u8; 4],
    pub out_score: bool,
    pub out_priority: bool,
    pub score_right: bool,
    pub vblank_seen: bool,
    pub vblank_latched: bool,
    // Inputs.
    /// pads I0-I5
    pub inputs: u8,
    pub trigger_latch: u8,
    /// board convenience: INPT4/INPT5 pressed
    pub fire: [bool; 2],
    /// when VBLANK D7 last let the paddle pads go (`now`)
    pub dump_released: u64,
    // Pins after the last step.
    pub ph0: bool,
    /// Where in the line: step 0 is the step HBLANK starts (a falling edge).
    pub step_in_line: u32,
    /// the next step's edge
    pub rising: bool,
}

impl Tia {
    pub fn new(hclock: u32) -> Self {
        let mut tia = Tia {
            line: 0,
            frame: 0,
            pixels: [0; 228],
            on_line: None,
            registers: [0; 0x40],
            audio: Audio::new(),
            vsync: false,
            vblank: 0,
            nusiz: [0; 2],
            missile_size: [0; 2],
            colours: [0; 4],
            ctrlpf: 0,
            reflect: [false; 2],
            pf: [0; 3],
            grp_new: [0; 2],
            grp_old: [0; 2],
            enam: [false; 2],
            enabl_new: false,
            enabl_old: false,
            hm: [0; 5],
            vdelp: [false; 2],
            vdelbl: false,
            resmp: [false; 2],
            hcount: 0,
            hphase: 3,
            hblank: true,
            hsync: false,
            rsync_window: 0,
            rsync_wait: 0,
            cpu_divider: 3,
            rdy_low: false,
            release_left: 0,
            wsync_left: 0,
            hmove_latch: false,
            revision: (0x1F, 0x1F),
            motion_count: 15,
            motion_more: 0,
            motion_armed: false,
            motion_running: false,
            h1_history: 0,
            h2_history: 0,
            hmove_history: 0,
            hm_late: [[0; 5]; 4],
            hm_pos: 0,
            merge_1: 0,
            merge_2: 0,
            merge_3: 0,
            merge_4: 0,
            merged: 0,
            counters_busy: false,
            pixels_cache: None,
            counters: Default::default(),
            players: Default::default(),
            missiles: Default::default(),
            ball: Width::default(),
            pf_index: 0,
            pf_down: false,
            pf_read: false,
            pf_shown: false,
            collisions: 0,
            out_objects: [false; 5],
            out_colours: [0; 4],
            out_score: false,
            out_priority: false,
            score_right: false,
            vblank_seen: false,
            vblank_latched: false,
            inputs: 0x3F,
            trigger_latch: 0b11,
            fire: [false; 2],
            dump_released: 0,
            ph0: true,
            step_in_line: 0,
            rising: true,
        };
        for _ in 0..2 * hclock {
            tia.step(None);
        }
        tia
    }

    // -- the board's view ---------------------------------------------------

    pub fn rdy(&self) -> bool {
        !self.rdy_low
    }

    /// Colour clock within the line, 0 at HBLANK's start.
    pub fn hclock(&self) -> u32 {
        self.step_in_line / 2
    }

    /// Half colour clocks since power-on, near enough for the paddles' RC
    /// (lines are counted whole; RSYNC's short line is not).
    pub fn now(&self) -> u64 {
        self.line * LINE_STEPS as u64 + self.step_in_line as u64
    }

    pub fn clocks_to_line_end(&self) -> u32 {
        (LINE_STEPS - self.step_in_line) / 2
    }

    /// Advance `clocks` colour clocks (two steps each), writing nothing.
    pub fn run(&mut self, clocks: u32) {
        for _ in 0..2 * clocks {
            self.step(None);
        }
    }

    /// A write on the step that is about to run.
    pub fn write(&mut self, address: u8, value: u8) {
        self.step(Some((address, value)));
    }

    // -- one half colour clock ---------------------------------------------

    /// One half colour clock; `write` = (address, value) when the 6507's Phi2
    /// falls on this step with a write to the TIA.
    ///
    /// Everything below reads the present state before anything is written
    /// back, so the order of the sections is free; objects whose clock did
    /// not tick and that hold no strobe skip their update (all of them
    /// through HBLANK and on every falling edge but HMOVE's), which is most
    /// steps.
    pub fn step(&mut self, write: Option<(u8, u8)>) {
        let rise = self.rising;
        let fall = !rise;
        let address: i32 = match write {
            Some((a, _)) => (a & 0x3F) as i32,
            None => -1,
        };

        // ---- horizontal counter (tia-fpga "Clocks and the horizontal counter")
        let (hcount, hphase, hblank) = (self.hcount, self.hphase, self.hblank);
        let h1 = fall && hphase == 0;
        let h2 = fall && hphase == 2;
        let rsync_go = self.rsync_wait == 1;
        let at_shb = hcount == SHB;
        let at_rhb = hcount == if self.hmove_latch { LRHB } else { RHB };
        let shb = (h2 && at_shb) || rsync_go;
        let release = rise && hphase == 1 && at_shb; // three steps before H@2
        let hblank_next = if at_shb || rsync_go {
            true
        } else if at_rhb {
            false
        } else {
            hblank
        };

        let (mut n_hcount, mut n_hphase, mut n_hblank) = (hcount, hphase, hblank);
        if fall {
            n_hphase = (hphase + 1) & 3;
        }
        if rsync_go {
            n_hcount = 0;
            n_hblank = true;
            n_hphase = 3;
        } else if h2 {
            n_hcount = if at_shb || hcount == LFSR_ERROR {
                0
            } else {
                next_count(hcount, 57)
            };
            if at_shb {
                n_hblank = true;
            } else if at_rhb {
                n_hblank = false;
            }
        }
        if rise && hphase == 1 {
            // the sync latch, three steps before H@2
            if hcount == SHS {
                self.hsync = true;
            } else if hcount == RHS {
                self.hsync = false;
            }
        }

        // RSYNC restarts the counter seven steps after the last step its strobe
        // shares with PH0 low (PH0 as it stood after the step before).
        let (rsync_window, rsync_wait) = (self.rsync_window, self.rsync_wait);
        if address == RSYNC {
            self.rsync_window = 3;
            self.rsync_wait = 0;
        } else if rsync_window != 0 || rsync_wait != 0 {
            self.rsync_window = if rsync_window != 0 {
                rsync_window - 1
            } else {
                0
            };
            if rsync_window != 0 && !self.ph0 {
                self.rsync_wait = 6;
            } else {
                self.rsync_wait = if rsync_wait != 0 { rsync_wait - 1 } else { 0 };
            }
        }

        // PH0: the colour clock divided by three on both edges, reloaded at
        // every line start (tia-fpga: "the horizontal counter reloads it").
        if shb {
            self.cpu_divider = 3;
        } else if rsync_wait == 3 {
            self.cpu_divider = 0;
        } else {
            self.cpu_divider = if self.cpu_divider == 5 {
                0
            } else {
                self.cpu_divider + 1
            };
        }

        // ---- RDY: set by WSYNC's strobe, released by an eight-step pulse that
        // starts three steps before HBLANK (tia-fpga "The bus").
        let (release_left, wsync_left) = (self.release_left, self.wsync_left);
        if release || release_left != 0 || address == WSYNC || wsync_left != 0 {
            let release_now = release || release_left != 0;
            let wsync_now = address == WSYNC || wsync_left != 0;
            self.release_left = if release {
                7
            } else if release_left != 0 {
                release_left - 1
            } else {
                0
            };
            self.wsync_left = if address == WSYNC {
                2
            } else if wsync_left != 0 {
                wsync_left - 1
            } else {
                0
            };
            if release_now && fall {
                self.rdy_low = false;
            } else if wsync_now && !release_now {
                self.rdy_low = true;
            }
        }

        // ---- HMOVE (Towers 604-753; tia-fpga "HMOVE"): the motion counter and
        // its comparators run 12 steps behind the horizontal counter, and the
        // comparators see HMxx as it stood four steps before: an HMxx write
        // just as the motion runs out still counts (die trace hm_late, BASIC
        // Programming; two steps missed it, five or more break tia-fpga's
        // hmove trace on its own wiring; milestone 4).
        let m_h1 = self.h1_history & 0x800 != 0;
        let m_h2 = self.h2_history & 0x800 != 0;
        let m_hmove = self.hmove_history & 0x800 != 0;
        let mut stuff: u8 = 0;
        let mut merge_now: u8 = 0;
        let count = self.motion_count;
        let mut still: u8 = 0;
        if m_h1 {
            let mut hit: u8 = 0;
            for (i, &hm) in self.hm_late[self.hm_pos].iter().enumerate() {
                let hm = hm as i32;
                if count == (hm & 0x8) | (!hm & 0x7) {
                    // D7 is wired the other way
                    hit |= 0x10 >> i;
                }
            }
            still = self.motion_more & !hit;
            if hblank_next {
                stuff = still;
            } else {
                merge_now = still;
            }
        }
        if m_hmove {
            self.motion_count = 15;
            self.motion_more = 0x1F;
            self.motion_armed = true;
            self.motion_running = true;
        } else if m_h1 {
            self.motion_more = still;
            self.motion_armed = false;
        } else if m_h2 && !self.motion_armed && self.motion_running {
            // Down to 0, then reset to 15, where it rests until the next HMOVE
            // (Towers: "64 CLK (including counter reset at the end)"): so after
            // an HMOVE no HMxx of $70 (compare value 0) can clear a stuck
            // latch, only another HMOVE can (hmove_cycles die trace).
            if count == 0 {
                self.motion_count = 15;
                self.motion_running = false;
            } else {
                self.motion_count = count - 1;
            }
        }
        let merge = self.merge_4;
        // The latch that moves HBLANK's end to LRHB samples the strobe on H@2,
        // the first one at least five steps after the write, and the counter's
        // wrap clears it: so an HMOVE on cycle 20 is too late for this line's
        // RHB (hmove_cycles die trace). When the set and the wrap's clear fall
        // on one half clock (an HMOVE on cycle 74) the set wins, as on seven
        // consoles (Bradford Mott's NTSC table, ale-79's six PAL/SECAM ones,
        // Stella #699), so the next line gets the comb; Sim2600's netlist
        // settles that race the other way (C27).
        let n_hmove_latch = if h2 && self.hmove_history & 0xFF0 != 0 {
            true // a write 5 to 12 steps ago
        } else if shb {
            false
        } else {
            self.hmove_latch
        };

        // ---- object clocks: MOTCK while HBLANK is off, plus stuffed pulses; a
        // merged pulse steps the object early and absorbs its next MOTCK.
        let motck = rise && !hblank;
        let (kept_strobe, steps) = self.revision;
        let merged = self.merged;
        let object_motck = (if motck { 0x1F & !merged } else { 0 }) | (merge & steps);
        let object_clock = object_motck | stuff;
        self.merged = merge | if motck { 0 } else { merged };

        // ---- objects: outputs from the present state (collisions, the pads)
        let need_pixels = fall || !hblank;
        let pixels = if need_pixels {
            match self.pixels_cache {
                Some(pixels) => pixels,
                None => {
                    let pixels = self.object_pixels();
                    self.pixels_cache = Some(pixels);
                    pixels
                }
            }
        } else {
            [false; 5]
        };
        // RESMPx holds the missile's ring like a RESxx window, from the step
        // its player's scan counter shows 1, with no delay: a 2-clock delay put
        // a released missile two clocks right of the die's (resmp_release die
        // trace, Frostbite's ice floes; milestone 4).
        let locks = [
            false,
            false,
            self.resmp[0] && self.players[0].scan == 1 && !self.players[0].copy_flag,
            self.resmp[1] && self.players[1].scan == 1 && !self.players[1].copy_flag,
            false,
        ];

        // Most steps (every falling edge without HMOVE's pulses, and HBLANK's
        // rising edges) clock no object and find none in a RESxx strobe, hold
        // or deferred step: then the sections below change nothing but each
        // player's NUSIZ history, so only that is done.
        let busy = object_clock != 0
            || (RESP0..RESP0 + 5).contains(&address)
            || locks[2]
            || locks[3]
            || self.counters_busy;
        if !busy {
            for i in 0..2 {
                let p = &mut self.players[i];
                p.size_history = (self.nusiz[i], p.size_history.0);
            }
        } else {
            // Ring events, from the present counter state:
            // (clocked, the end of H@1, H@2, held, ring).
            let mut events = [(false, false, false, false, 0u8); 5];
            for (i, event) in events.iter_mut().enumerate() {
                let c = &self.counters[i];
                let clocked = object_clock & (0x10 >> i) != 0;
                let held = c.window_history & 4 != 0 || locks[i];
                let ring = c.ring;
                let due = c.deferred && c.window_history == 0 && c.strobe_left == 0;
                let ring_clock = (clocked || due) && !held;
                *event = (
                    clocked,
                    ring_clock && ring == 1,
                    ring_clock && ring == 2,
                    held,
                    ring,
                );
            }

            // ---- players
            for i in 0..2 {
                let (clocked, o_h1_end, o_h2, held, ring) = events[i];
                let nusiz = self.nusiz[i];
                let count = self.counters[i].count;
                let p = &mut self.players[i];
                let (size1, size2) = p.size_history;
                if !clocked
                    && !o_h1_end
                    && size1 == nusiz
                    && size2 == nusiz
                    && object_motck & (0x10 >> i) == 0
                {
                    continue;
                }
                let mut start_decode = false;
                if o_h1_end {
                    start_decode = count == MAIN || copies(nusiz, count);
                    if start_decode && count != MAIN {
                        p.copy_flag = true;
                    }
                }
                if o_h2 && count == MAIN {
                    p.copy_flag = false;
                }
                let (old_gate, old_q, old_q2) = (p.gate, p.start_q, p.start_q2);
                if clocked || object_motck & (0x10 >> i) != 0 {
                    // The stretch gate: the two-phase state caught on MOTCK and on
                    // HMOVE's stuffed pulses too, so under the comb those shift out
                    // a wrapped player's last pixels (Towers: "a HMOVE can gobble up
                    // the wrapped player graphics"; die trace hmove_wrap, Sea Hawk),
                    // NUSIZ taken two steps late (tia-fpga "Objects"). A RESxx hold
                    // opens a 2x gate and shuts a 4x one (resp_start).
                    p.gate = if size2 == 0b111 {
                        held || ring != 3
                    } else if size2 == 0b101 {
                        !(held || ring == 1 || ring == 3)
                    } else {
                        false
                    };
                }
                if clocked {
                    let start_pulse = if nusiz == 0b101 || nusiz == 0b111 {
                        old_q2
                    } else {
                        old_q
                    };
                    p.start_q = o_h2 && p.start_latched;
                    p.start_q2 = old_q;
                    // The scan counter takes START only on a clock the stretch gate
                    // lets through: a 4x copy whose start falls in a RESxx hold is
                    // never drawn, a 2x one is (die traces hmove_wrap, resp_start).
                    if start_pulse && !old_gate {
                        p.scan = 0;
                        p.scanning = true;
                    } else if !old_gate && p.scanning {
                        p.scanning = p.scan != 7;
                        p.scan = (p.scan + 1) & 7;
                    }
                }
                if o_h1_end {
                    p.start_latched = start_decode;
                }
                p.size_history = (nusiz, size1);
            }

            // ---- missiles and the ball
            for j in 0..2 {
                let (_, o_h1_end, o_h2, _, _) = events[2 + j];
                if o_h1_end {
                    let count = self.counters[2 + j].count;
                    self.missiles[j].start_latched = count == MAIN || copies(self.nusiz[j], count);
                } else if o_h2 {
                    let m = &mut self.missiles[j];
                    m.second = m.first;
                    m.first = m.start_latched;
                }
            }
            if events[4].2 {
                let first = self.ball.first;
                self.ball.second = first;
                self.ball.first = self.counters[4].clear_pending;
            }

            // ---- object counters (after the players, missiles and ball read them)
            for i in 0..5 {
                let strobe = address == RESP0 + i as i32;
                let (clocked, o_h1_end, o_h2, held, ring) = events[i];
                let c = &mut self.counters[i];
                if !(clocked
                    || strobe
                    || c.strobe_left != 0
                    || c.window_history != 0
                    || held
                    || c.deferred)
                {
                    continue;
                }
                if o_h1_end {
                    c.clear_pending = c.pending || c.count == MAIN || c.count == LFSR_ERROR;
                }
                if o_h2 {
                    c.count = if c.clear_pending {
                        0
                    } else {
                        next_count(c.count, 40)
                    };
                }
                // A stuffed (HMOVE) pulse that arrives after the RESxx strobe's own
                // step and before its hold lets go is not lost: it steps the ring
                // once the hold is over. MOTCK pulses, on the other edge, never meet
                // the strobe's steps (hmove_resp0 die trace; C28).
                let due = c.deferred && c.window_history == 0 && c.strobe_left == 0 && !strobe;
                c.ring = if held {
                    1
                } else if clocked || due {
                    (ring + 1) & 3
                } else {
                    ring
                };
                let kept = if c.strobe_left != 0 {
                    kept_strobe
                } else {
                    0x1F
                }; // the hold always keeps it
                if stuff & kept & (0x10 >> i) != 0 && (c.strobe_left != 0 || held) {
                    c.deferred = true;
                } else if due {
                    c.deferred = false;
                }
                c.pending = if held {
                    true
                } else if o_h2 {
                    false
                } else {
                    c.pending
                };
                let window = strobe || c.strobe_left != 0;
                c.strobe_left = if strobe {
                    2
                } else if c.strobe_left != 0 {
                    c.strobe_left - 1
                } else {
                    0
                };
                c.window_history = ((c.window_history << 1) | window as u8) & 0b111;
            }
            self.pixels_cache = None;
            self.counters_busy = self
                .counters
                .iter()
                .any(|c| c.strobe_left != 0 || c.window_history != 0 || c.deferred);
        }

        // ---- playfield: each bit is read on H@1 and shown from the next H@2
        // (tia-fpga "The picture"). The bit is chosen by the horizontal count
        // alone, from RHB even on a line HMOVE's comb blanks to LRHB, so the
        // comb hides the first two bits but does not delay the rest (Pitfall!'s
        // trunks; die trace hmove_playfield, milestone 4).
        let pf_pixel = self.pf_shown && !hblank;
        if h1 {
            let mut index_next = if hcount == RHB {
                0
            } else if hcount == CNT {
                if self.ctrlpf & 1 != 0 {
                    19
                } else {
                    0
                }
            } else if self.pf_down {
                self.pf_index - 1
            } else {
                self.pf_index + 1
            };
            if !(0..=19).contains(&index_next) {
                index_next = 0;
            }
            let (register, bit) = PF_BITS[index_next as usize];
            self.pf_read = (self.pf[register] >> bit) & 1 != 0;
        }
        if h2 {
            self.pf_shown = self.pf_read;
            if hcount == RHB {
                self.pf_index = 0;
                self.pf_down = false;
            } else if hcount == CNT {
                if self.ctrlpf & 1 != 0 {
                    self.pf_index = 19;
                    self.pf_down = true;
                } else {
                    self.pf_index = 0;
                    self.pf_down = false;
                }
            } else {
                self.pf_index += if self.pf_down { -1 } else { 1 };
            }
        }

        // ---- collisions, on the visible rising edges; VBLANK gates them too
        // (this project's vblank_collisions die trace, C24)
        if address == CXCLR {
            self.collisions = 0;
        } else if rise && !hblank && !self.vblank_latched {
            let [p0, p1, m0, m1, bl] = pixels;
            let pf = pf_pixel;
            let lit = p0 as u8 + p1 as u8 + m0 as u8 + m1 as u8 + bl as u8 + pf as u8;
            if lit >= 2 {
                let b = |x: bool, shift: u32| (x as u16) << shift;
                self.collisions |= b(m0 && p1, 0)
                    | b(m0 && p0, 1)
                    | b(m1 && p0, 2)
                    | b(m1 && p1, 3)
                    | b(p0 && pf, 4)
                    | b(p0 && bl, 5)
                    | b(p1 && pf, 6)
                    | b(p1 && bl, 7)
                    | b(m0 && pf, 8)
                    | b(m0 && bl, 9)
                    | b(m1 && pf, 10)
                    | b(m1 && bl, 11)
                    | b(bl && pf, 12)
                    | b(p0 && p1, 13)
                    | b(m0 && m1, 14);
            }
        }

        // ---- output latches: objects and colours on the falling edge; an
        // object that took a merged pulse is latched just after it steps,
        // below. The playfield and blanking reach the pads directly (tia-fpga
        // "The picture").
        if fall {
            for (i, &pixel) in pixels.iter().enumerate() {
                if merge & (0x10 >> i) == 0 {
                    self.out_objects[i] = pixel;
                }
            }
            // A colour register written on this step reaches the latch on this
            // same falling edge: the die shows a mid-line COLUBK write on the
            // write's own half clock (tests/die_traces retrigger, step 184).
            self.out_colours = self.colours;
            if (COLUP0..=COLUBK).contains(&address) {
                self.out_colours[(address - COLUP0) as usize] = write.unwrap().1;
            }
            self.out_score = self.ctrlpf & 2 != 0;
            self.out_priority = self.ctrlpf & 4 != 0;
        }
        if shb {
            self.score_right = false;
        } else if fall && hphase == 1 && hcount == CNT {
            // two steps before H@2
            self.score_right = true;
        }
        // VBLANK's bit settles a step after the write, then waits for a falling edge.
        if fall {
            self.vblank_latched = self.vblank_seen;
        }
        self.vblank_seen = self.vblank & 2 != 0;

        // ---- inputs: VBLANK D6 latches the triggers once low.
        if self.vblank & 0x40 != 0 {
            let fire = (self.fire[0] as u8) | ((self.fire[1] as u8) << 1);
            let pads = (self.inputs >> 4) & !fire;
            self.trigger_latch &= pads;
        } else {
            self.trigger_latch = 0b11;
        }

        // ---- commit the rest
        self.rising = !rise;
        self.hcount = n_hcount;
        self.hphase = n_hphase;
        self.hblank = n_hblank;
        self.hmove_latch = n_hmove_latch;
        self.h1_history = ((self.h1_history << 1) | h1 as u16) & 0xFFF;
        self.h2_history = ((self.h2_history << 1) | h2 as u16) & 0xFFF;
        self.hmove_history = ((self.hmove_history << 1) | (address == HMOVE) as u16) & 0xFFF;
        self.hm_late[self.hm_pos] = self.hm;
        self.hm_pos = (self.hm_pos + 1) & 3;
        self.merge_4 = self.merge_3;
        self.merge_3 = self.merge_2;
        self.merge_2 = self.merge_1;
        self.merge_1 = merge_now;
        if merge != 0 {
            // A merged pulse steps the object half a colour clock early, and the
            // pads take it on this same step (tia-fpga "HMOVE": "the object
            // shows its next state half a colour clock early").
            let now = self.object_pixels();
            for (i, &pixel) in now.iter().enumerate() {
                if merge & (0x10 >> i) != 0 {
                    self.out_objects[i] = pixel;
                }
            }
        }
        if let Some((_, value)) = write {
            self.write_register(address, value);
            self.pixels_cache = None;
        }
        if shb {
            if let Some(sink) = &mut self.on_line {
                sink(&self.pixels, self.vsync);
            }
            self.step_in_line = 0;
            self.line += 1;
        } else {
            self.step_in_line += 1;
        }

        // ---- sound: two ticks a line, each in two phases, on the rising edge
        // that latches the sync decodes; the pads change at phase B, steps 77
        // and 301 of the line, 112 and 116 colour clocks apart (die traces
        // `audio` and `audio_modes`).
        if rise && hphase == 1 {
            if AUDIO_A.contains(&hcount) {
                self.audio.tick_a();
            } else if AUDIO_B.contains(&hcount) {
                self.audio.tick_b();
            }
        }

        // ---- pins, from the committed state. PH0 is kept every step (RSYNC
        // reads it on the next); the other pins only matter to a reader of
        // the die's pins and are computed when asked (`pins`), and the colour
        // only on the falling edge that takes a pixel.
        self.ph0 = self.cpu_divider >= 3;
        if fall {
            let blank = self.hblank || self.vblank_latched;
            self.pixels[((self.step_in_line >> 1) % 228) as usize] =
                if blank { 0 } else { (self.colour() & 0xFE) | 1 };
        }
    }

    /// The colour the output latches select, as the pads drive it unblanked.
    fn colour(&self) -> u8 {
        let [p0q, p1q, m0q, m1q, blq] = self.out_objects;
        let [colup0, colup1, colupf, colubk] = self.out_colours;
        let pf_or_ball = self.pf_shown || blq;
        let pf_colour = if self.out_score && !self.out_priority {
            if self.score_right {
                colup1
            } else {
                colup0
            }
        } else {
            colupf
        };
        if self.out_priority && pf_or_ball {
            pf_colour
        } else if p0q || m0q {
            colup0
        } else if p1q || m1q {
            colup1
        } else if self.pf_shown {
            pf_colour
        } else if blq {
            // SCORE colours the playfield only: the ball keeps COLUPF
            // (C32; die trace ball_score, Alpha Beam with Ernie).
            colupf
        } else {
            colubk
        }
    }

    /// The pins after the last step, from the committed state (the
    /// reference keeps them as fields, updated every step).
    pub fn pins(&self) -> Pins {
        let [c0, c1] = &self.audio.channels;
        let blank = self.hblank || self.vblank_latched;
        let colour = if blank { 0 } else { self.colour() };
        Pins {
            au0: if c0.pulse & 1 != 0 { c0.audv } else { 0 },
            au1: if c1.pulse & 1 != 0 { c1.audv } else { 0 },
            sync_low: self.hsync != self.vsync,
            blank,
            col: colour >> 4,
            lum: (colour >> 1) & 7,
        }
    }

    /// Each object's output as the present state gives it.
    fn object_pixels(&self) -> [bool; 5] {
        let mut out = [false; 5];
        for i in 0..2 {
            let p = &self.players[i];
            let grp = if self.vdelp[i] {
                self.grp_old[i]
            } else {
                self.grp_new[i]
            };
            let bit = if self.reflect[i] { p.scan } else { 7 - p.scan };
            out[i] = p.scanning && (grp >> bit) & 1 != 0;
        }
        for j in 0..2 {
            let ring = self.counters[2 + j].ring;
            out[2 + j] = width_active(&self.missiles[j], self.missile_size[j], ring)
                && self.enam[j]
                && !self.resmp[j];
        }
        let enabl = if self.vdelbl {
            self.enabl_old
        } else {
            self.enabl_new
        };
        out[4] = width_active(&self.ball, (self.ctrlpf >> 4) & 3, self.counters[4].ring) && enabl;
        out
    }

    fn write_register(&mut self, address: i32, value: u8) {
        self.registers[address as usize] = value;
        match address {
            VSYNC => {
                let on = value & 0x02 != 0;
                if on && !self.vsync {
                    self.frame += 1;
                }
                self.vsync = on;
            }
            VBLANK => {
                if self.vblank & 0x80 != 0 && value & 0x80 == 0 {
                    self.dump_released = self.now();
                }
                self.vblank = value;
            }
            NUSIZ0 | NUSIZ1 => {
                let i = (address - NUSIZ0) as usize;
                self.nusiz[i] = value & 7;
                self.missile_size[i] = (value >> 4) & 3;
            }
            COLUP0..=COLUBK => self.colours[(address - COLUP0) as usize] = value,
            CTRLPF => self.ctrlpf = value,
            REFP0 | REFP1 => self.reflect[(address - REFP0) as usize] = value & 0x08 != 0,
            PF0..=PF2 => self.pf[(address - PF0) as usize] = value,
            GRP0 => {
                self.grp_new[0] = value;
                self.grp_old[1] = self.grp_new[1];
            }
            GRP1 => {
                self.grp_new[1] = value;
                self.grp_old[0] = self.grp_new[0];
                self.enabl_old = self.enabl_new;
            }
            ENAM0 | ENAM1 => self.enam[(address - ENAM0) as usize] = value & 0x02 != 0,
            ENABL => self.enabl_new = value & 0x02 != 0,
            HMP0..=HMBL => self.hm[(address - HMP0) as usize] = value >> 4,
            VDELP0 | VDELP1 => self.vdelp[(address - VDELP0) as usize] = value & 1 != 0,
            VDELBL => self.vdelbl = value & 1 != 0,
            RESMP0 | RESMP1 => self.resmp[(address - RESMP0) as usize] = value & 0x02 != 0,
            HMCLR => self.hm = [0; 5],
            AUDC0..=AUDV1 => self.audio.write(address as u8, value),
            _ => {}
        }
    }

    // -- reads ----------------------------------------------------------------

    /// D7 and D6 of a read, with the pads as `inputs` gives them.
    pub fn read(&self, address: u8) -> u8 {
        self.read_with(address, self.inputs)
    }

    /// D7 and D6 of a read, as the die drives them during Phi2; D5-D0 float.
    ///
    /// The two read addresses with no register ($E, $F) are driven as zeros
    /// (tia-fpga "The bus"). VBLANK D7 grounds the paddle pads; VBLANK D6
    /// makes the trigger pads latch low. `pads` is I0-I5 as the controllers
    /// present them at this read (the reference's `pad_source`).
    pub fn read_with(&self, address: u8, pads: u8) -> u8 {
        let address = address & 0x0F;
        if address < 8 {
            let (d7_bit, d6_bit) = COLLISION_READS[address as usize];
            let c = self.collisions;
            let d6 = match d6_bit {
                None => 0,
                Some(bit) => (c >> bit) & 1,
            };
            return ((((c >> d7_bit) & 1) << 7) | (d6 << 6)) as u8;
        }
        if address < 12 {
            return if self.vblank & 0x80 != 0 {
                0
            } else {
                ((pads >> (address - 8)) & 1) << 7
            };
        }
        if address < 14 {
            let i = (address - 12) as usize;
            let mut pad = if self.fire[i] {
                0
            } else {
                (pads >> (4 + i)) & 1
            };
            if self.vblank & 0x40 != 0 {
                pad &= self.trigger_latch >> i;
            }
            return (pad & 1) << 7;
        }
        0
    }
}
