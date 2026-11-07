//! A desktop window: play a cartridge with picture, sound, keyboard, mouse and
//! gamepads (the reference's `frontend.py`, on SDL2 instead of pygame).
//!
//!     a2600-play "Pitfall! - Pitfall Harry's Jungle Adventure (USA).zip"
//!     a2600-play game.bin --machine pal --left paddles --phosphor
//!
//! Picture: the television's frames, a fixed window of lines per standard, in
//! a palette from `palettes` (the community's charts, not measurements).
//! `--phosphor` averages each frame with the one before, a frontend option,
//! never a board behaviour. Sound: the TIA's two samples a line, resampled
//! linearly to 44.1 kHz; the queue sets the pace, as the board runs faster
//! than real time (without sound, a timer at the machine's frame rate does).
//! SaveKey: plugged into the right port with `--right savekey`, read at power
//! on and written on quit, in `--savekey PATH` or else
//! `~/.local/share/a2600-python/savekey/<rom>.bin` (never beside the ROM).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use a2600_rust::cartridge::{read_image, Options};
use a2600_rust::controllers::{
    booster_grip, joystick, Driving, Keypads, Paddles, Pads, SaveKey, Stick,
};
use a2600_rust::machine::machine;
use a2600_rust::palettes::palette;
use a2600_rust::riot::PortA;
use a2600_rust::television::{frame_png, Television};
use a2600_rust::System;
use sdl2::audio::{AudioQueue, AudioSpecDesired};
use sdl2::controller::{Axis, Button, GameController};
use sdl2::event::Event;
use sdl2::keyboard::Scancode;
use sdl2::pixels::PixelFormatEnum;

const SAMPLE_RATE: i32 = 44_100;
const BUFFER_FRAMES: f64 = 3.0;
const WIDTH: usize = 160;

const HELP: &str = "\
Arrows / Space   left joystick, fire     W A S D / Left Shift  right joystick, fire
F1 Select   F2 Reset   F3 Colour   F4 B/W   F5/F6 left difficulty A/B   F7/F8 right A/B
Paddles: the mouse's x moves paddle 0, its button fires; Driving: Left/Right turn
Keypad: 1 2 3 / Q W E / A S D / Z X C      Booster Grip: Z booster, X trigger
P pause   Tab (hold) fast forward   F11 fullscreen   F12 screenshot (PNG)   Esc quit
Gamepads: the first is the left joystick, the second the right.";

// SWCHB: D0 Reset, D1 Select (pressed = 0), D3 colour (1) / B/W (0), D6/D7 difficulty A (1)
const RESET: u8 = 0x01;
const SELECT: u8 = 0x02;
const COLOUR: u8 = 0x08;
const LEFT_A: u8 = 0x40;
const RIGHT_A: u8 = 0x80;

/// (first line shown, lines shown) of a television frame, per standard.
fn window_lines(machine: &str) -> (usize, usize) {
    match machine {
        "ntsc" => (28, 216),
        _ => (36, 256),
    }
}

/// Linear interpolation from the TIA's rate to the device's, keeping its phase.
struct Resampler {
    step: f64,
    position: f64,
    last: f64,
}

impl Resampler {
    fn new(source_hz: f64, target_hz: f64) -> Self {
        Resampler {
            step: source_hz / target_hz,
            position: 0.0,
            last: 0.0,
        }
    }

    fn resample(&mut self, samples: &[f64]) -> Vec<f64> {
        let mut data = Vec::with_capacity(samples.len() + 1);
        data.push(self.last);
        data.extend_from_slice(samples);
        let mut out = Vec::new();
        let mut position = self.position;
        while position + 1.0 < data.len() as f64 {
            let i = position as usize;
            let frac = position - i as f64;
            out.push(data[i] + (data[i + 1] - data[i]) * frac);
            position += self.step;
        }
        self.position = position - (data.len() - 1) as f64;
        if let Some(&last) = samples.last() {
            self.last = last;
        }
        out
    }
}

/// The TIA's output (0 to 1) centred, as signed 16-bit mono.
fn samples_s16(samples: &[f64], gain: f64) -> Vec<i16> {
    samples
        .iter()
        .map(|x| (((x - 0.5) * 2.0 * gain * 32767.0) as i64).clamp(-32768, 32767) as i16)
        .collect()
}

/// Phosphor: each channel the mean of this frame's and the last's.
fn blend(current: &mut [u32], previous: &[u32]) {
    for (a, &b) in current.iter_mut().zip(previous) {
        // each halved first, so no channel carries into the next
        *a = ((*a & 0xFEFEFE) >> 1) + ((b & 0xFEFEFE) >> 1) | 0xFF00_0000;
    }
}

/// The SaveKey as the RIOT's device, kept so its data can be written on quit.
struct SharedSaveKey(Rc<RefCell<SaveKey>>);

impl PortA for SharedSaveKey {
    fn lines(&mut self, levels: u8, cycle: i64) -> u8 {
        self.0.borrow_mut().lines(levels, cycle)
    }
}

fn save_path(rom: &Path) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/share/a2600-python/savekey").join(format!(
        "{}.bin",
        rom.file_name().unwrap().to_string_lossy()
    ))
}

struct Args {
    rom: PathBuf,
    machine: String,
    left: String,
    right: String,
    scale: u32,
    phosphor: bool,
    audio: bool,
    fullscreen: bool,
    savekey: Option<PathBuf>,
    frames: Option<u64>,
}

fn parse() -> Args {
    let mut args = Args {
        rom: PathBuf::new(),
        machine: "ntsc".into(),
        left: "joystick".into(),
        right: "joystick".into(),
        scale: 3,
        phosphor: false,
        audio: true,
        fullscreen: false,
        savekey: None,
        frames: None,
    };
    let mut rom = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{arg} needs a value"));
        match arg.as_str() {
            "--machine" => args.machine = value(),
            "--left" => args.left = value(),
            "--right" => args.right = value(),
            "--scale" => args.scale = value().parse::<u32>().expect("a number").max(1),
            "--phosphor" => args.phosphor = true,
            "--no-audio" => args.audio = false,
            "--fullscreen" => args.fullscreen = true,
            "--savekey" => args.savekey = Some(PathBuf::from(value())),
            "--frames" => args.frames = Some(value().parse().expect("a number")),
            "-h" | "--help" => {
                println!("usage: a2600-play ROM [--machine ntsc|pal|secam] [--left joystick|paddles|keypad|driving|booster]\n                  [--right joystick|savekey|none] [--scale N] [--phosphor] [--no-audio] [--fullscreen]\n                  [--savekey PATH] [--frames N]\n\n{HELP}");
                std::process::exit(0);
            }
            _ => rom = Some(PathBuf::from(arg)),
        }
    }
    args.rom = rom.expect("usage: a2600-play ROM [options] (--help)");
    for (kind, allowed) in [
        (
            &args.left,
            &["joystick", "paddles", "keypad", "driving", "booster"][..],
        ),
        (&args.right, &["joystick", "savekey", "none"][..]),
    ] {
        if !allowed.contains(&kind.as_str()) {
            panic!("{kind} is not one of {allowed:?}");
        }
    }
    args
}

fn main() {
    let args = parse();
    println!("{HELP}");
    let row = machine(&args.machine).unwrap_or_else(|| panic!("no machine {}", args.machine));
    let (image, _) = read_image(&args.rom).unwrap_or_else(|e| panic!("{e}"));
    let mut system =
        System::from_rom(&image, row, None, &Options::default()).unwrap_or_else(|e| panic!("{e}"));
    let television = Television::attach(&mut system.board_mut().tia);
    let mut switches = row.switches;
    let mut driving = None;
    {
        let board = system.board_mut();
        board.riot.input_b = switches;
        match args.left.as_str() {
            "paddles" => board.pads = Pads::Paddles(Paddles::new(row.timing.colour_clock_hz)),
            "keypad" => board.pads = Pads::Keypads(Keypads::default()),
            "driving" => driving = Some(Driving::new(&mut board.riot, 0)),
            "booster" => board.pads = booster_grip(Pads::None, 0),
            _ => {}
        }
    }
    let savekey_path = args.savekey.clone().unwrap_or_else(|| save_path(&args.rom));
    let savekey = (args.right == "savekey").then(|| {
        let data = std::fs::read(&savekey_path).ok();
        let key = Rc::new(RefCell::new(
            SaveKey::new(1, data, row.timing.cpu_hz()).unwrap_or_else(|e| panic!("{e}")),
        ));
        system.board_mut().riot.port_a_device = Some(Box::new(SharedSaveKey(Rc::clone(&key))));
        key
    });

    sdl2::hint::set("SDL_GAMECONTROLLER_USE_BUTTON_LABELS", "0"); // positions, not labels
    let sdl = sdl2::init().expect("SDL");
    let video = sdl.video().expect("SDL video");
    let controllers = sdl.game_controller().expect("SDL game controllers");
    let (first, lines) = window_lines(&args.machine);
    let mut builder = video.window(
        &format!(
            "{} — a2600-rust-fast",
            args.rom.file_stem().unwrap().to_string_lossy()
        ),
        2 * WIDTH as u32 * args.scale,
        lines as u32 * args.scale,
    );
    builder.position_centered().resizable();
    if args.fullscreen {
        builder.fullscreen_desktop();
    }
    let window = builder.build().expect("a window");
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .expect("a canvas");
    let creator = canvas.texture_creator();
    let mut texture = creator
        .create_texture_streaming(PixelFormatEnum::ARGB8888, WIDTH as u32, lines as u32)
        .expect("a texture");
    let audio: Option<AudioQueue<i16>> = if args.audio {
        sdl.audio().ok().and_then(|subsystem| {
            let spec = AudioSpecDesired {
                freq: Some(SAMPLE_RATE),
                channels: Some(1),
                samples: Some(512),
            };
            match subsystem.open_queue::<i16, _>(None, &spec) {
                Ok(queue) => {
                    queue.resume();
                    Some(queue)
                }
                Err(error) => {
                    eprintln!("no sound: {error}");
                    None
                }
            }
        })
    } else {
        None
    };
    let mut events = sdl.event_pump().expect("events");
    let colours: Vec<u32> = palette(&args.machine)
        .expect("a palette")
        .iter()
        .map(|[r, g, b]| 0xFF00_0000 | (*r as u32) << 16 | (*g as u32) << 8 | *b as u32)
        .collect();
    let frame_rate = row.timing.colour_clock_hz / (228.0 * row.timing.nominal_lines as f64);
    let mut resampler =
        Resampler::new(2.0 * row.timing.colour_clock_hz / 228.0, SAMPLE_RATE as f64);
    let target = (BUFFER_FRAMES * SAMPLE_RATE as f64 / frame_rate) as u32 * 2; // bytes
    let mut pads: Vec<GameController> = Vec::new();
    let (mut paused, mut fast) = (false, false);
    let mut previous: Option<Vec<u32>> = None;
    let mut words = vec![0u32; WIDTH * lines];
    let (mut frames, mut since, mut shown) = (0u32, Instant::now(), 0u64);
    let mut next_frame = Instant::now();

    'running: loop {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => break 'running,
                Event::ControllerDeviceAdded { which, .. } => {
                    if let Ok(pad) = controllers.open(which) {
                        pads.push(pad);
                    }
                }
                Event::ControllerDeviceRemoved { which, .. } => {
                    pads.retain(|p| p.instance_id() != which)
                }
                Event::KeyDown {
                    scancode: Some(key),
                    repeat: false,
                    ..
                } => match key {
                    Scancode::Escape => break 'running,
                    Scancode::F3 => switches |= COLOUR,
                    Scancode::F4 => switches &= !COLOUR,
                    Scancode::F5 => switches |= LEFT_A,
                    Scancode::F6 => switches &= !LEFT_A,
                    Scancode::F7 => switches |= RIGHT_A,
                    Scancode::F8 => switches &= !RIGHT_A,
                    Scancode::P => {
                        paused = !paused;
                        if let Some(queue) = &audio {
                            queue.clear();
                        }
                    }
                    Scancode::F11 => {
                        let window = canvas.window_mut();
                        let state = window.fullscreen_state();
                        let next = if state == sdl2::video::FullscreenType::Off {
                            sdl2::video::FullscreenType::Desktop
                        } else {
                            sdl2::video::FullscreenType::Off
                        };
                        let _ = window.set_fullscreen(next);
                    }
                    Scancode::F12 => {
                        if let Some(frame) = &television.borrow().frame {
                            let _ = std::fs::create_dir_all("screenshots");
                            let stamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs());
                            let path = format!(
                                "screenshots/{}-{stamp}.png",
                                args.rom.file_stem().unwrap().to_string_lossy()
                            );
                            if std::fs::write(&path, frame_png(frame, 2)).is_ok() {
                                println!("saved {path}");
                            }
                        }
                    }
                    Scancode::Tab => {
                        fast = true;
                        if let Some(queue) = &audio {
                            queue.clear();
                        }
                    }
                    _ => {}
                },
                Event::KeyUp {
                    scancode: Some(Scancode::Tab),
                    ..
                } => fast = false,
                _ => {}
            }
        }

        let waiting = match &audio {
            Some(queue) if !fast => queue.size() >= target,
            None if !fast => Instant::now() < next_frame,
            _ => false,
        };
        if paused || waiting {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        next_frame += Duration::from_secs_f64(1.0 / frame_rate);
        if Instant::now() > next_frame + Duration::from_millis(100) {
            next_frame = Instant::now(); // fell behind: do not try to catch up
        }

        for _ in 0..if fast { 4 } else { 1 } {
            // -- inputs, as the reference's _apply_inputs
            let keys = events.keyboard_state();
            let key = |k| keys.is_scancode_pressed(k);
            let stick = |index: usize| -> Stick {
                let Some(pad) = pads.get(index) else {
                    return Stick::default();
                };
                let x = pad.axis(Axis::LeftX) as f64 / 32768.0;
                let y = pad.axis(Axis::LeftY) as f64 / 32768.0;
                Stick {
                    up: y < -0.5 || pad.button(Button::DPadUp),
                    down: y > 0.5 || pad.button(Button::DPadDown),
                    left: x < -0.5 || pad.button(Button::DPadLeft),
                    right: x > 0.5 || pad.button(Button::DPadRight),
                    fire: pad.button(Button::A),
                }
            };
            let pad0 = stick(0);
            let left_stick = Stick {
                up: key(Scancode::Up) || pad0.up,
                down: key(Scancode::Down) || pad0.down,
                left: key(Scancode::Left) || pad0.left,
                right: key(Scancode::Right) || pad0.right,
                fire: key(Scancode::Space) || pad0.fire,
            };
            let mouse = events.mouse_state();
            let width = canvas.window().size().0.max(1) as f64;
            let board = system.board_mut();
            match args.left.as_str() {
                "joystick" => joystick(&mut board.riot, &mut board.tia, 0, left_stick),
                "booster" => {
                    joystick(&mut board.riot, &mut board.tia, 0, left_stick);
                    if let Pads::Booster(buttons, _) = &mut board.pads {
                        buttons.booster = key(Scancode::Z);
                        buttons.trigger = key(Scancode::X);
                    }
                }
                "driving" => {
                    let d = driving.as_mut().unwrap();
                    d.turn(
                        &mut board.riot,
                        left_stick.right as i64 - left_stick.left as i64,
                    );
                    d.press(&mut board.tia, left_stick.fire);
                }
                "paddles" => {
                    if let Pads::Paddles(p) = &mut board.pads {
                        p.position[0] = Some((mouse.x() as f64 / width).clamp(0.0, 1.0));
                    }
                    let pressed = mouse.left() || left_stick.fire;
                    let level = if pressed {
                        board.riot.input_a & !0x80
                    } else {
                        board.riot.input_a | 0x80
                    };
                    board.riot.set_input_a(level);
                }
                "keypad" => {
                    if let Pads::Keypads(k) = &mut board.pads {
                        let rows = [
                            [Scancode::Num1, Scancode::Num2, Scancode::Num3],
                            [Scancode::Q, Scancode::W, Scancode::E],
                            [Scancode::A, Scancode::S, Scancode::D],
                            [Scancode::Z, Scancode::X, Scancode::C],
                        ];
                        k.pressed[0].clear();
                        for (r, row) in rows.iter().enumerate() {
                            for (c, &code) in row.iter().enumerate() {
                                if key(code) {
                                    k.pressed[0].push((r as u8, c as u8));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            if args.right == "joystick" {
                let pad1 = stick(1);
                let right_stick = Stick {
                    up: key(Scancode::W) || pad1.up,
                    down: key(Scancode::S) || pad1.down,
                    left: key(Scancode::A) || pad1.left,
                    right: key(Scancode::D) || pad1.right,
                    fire: key(Scancode::LShift) || pad1.fire,
                };
                joystick(&mut board.riot, &mut board.tia, 1, right_stick);
            }
            let mut input_b = switches | RESET | SELECT;
            if key(Scancode::F2) {
                input_b &= !RESET;
            }
            if key(Scancode::F1) {
                input_b &= !SELECT;
            }
            board.riot.input_b = input_b;

            // -- one frame
            system.run_frames(1);
            let samples = system.board_mut().tia.audio.take_samples();
            if let Some(queue) = &audio {
                if !fast {
                    let _ = queue.queue_audio(&samples_s16(&resampler.resample(&samples), 0.8));
                }
            }
            frames += 1;
        }

        // -- the picture
        if let Some(frame) = &television.borrow().frame {
            for (n, out) in words.chunks_mut(WIDTH).enumerate() {
                match frame.get(first + n) {
                    Some(line) => {
                        for (word, &value) in out.iter_mut().zip(line) {
                            *word = colours[value as usize];
                        }
                    }
                    None => out.fill(0xFF00_0000),
                }
            }
            if args.phosphor {
                let current = words.clone();
                if let Some(previous) = &previous {
                    blend(&mut words, previous);
                }
                previous = Some(current);
            }
            texture
                .with_lock(None, |buffer, pitch| {
                    for (y, row) in words.chunks(WIDTH).enumerate() {
                        for (x, &word) in row.iter().enumerate() {
                            let at = y * pitch + 4 * x;
                            buffer[at..at + 4].copy_from_slice(&word.to_le_bytes());
                        }
                    }
                })
                .expect("the texture");
            canvas.copy(&texture, None, None).expect("draw");
            canvas.present();
            shown += 1;
        }
        let now = Instant::now();
        if now - since >= Duration::from_secs(1) {
            let fps = frames as f64 / (now - since).as_secs_f64();
            let state = if paused {
                " (paused)"
            } else if fast {
                " (fast)"
            } else {
                ""
            };
            let title = format!(
                "{} — {fps:.1} fps{state} — a2600-rust-fast",
                args.rom.file_stem().unwrap().to_string_lossy()
            );
            let _ = canvas.window_mut().set_title(&title);
            frames = 0;
            since = now;
        }
        if args.frames.is_some_and(|limit| shown >= limit) {
            break;
        }
    }
    if let Some(key) = savekey {
        if let Some(folder) = savekey_path.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let _ = std::fs::write(&savekey_path, &key.borrow().data);
    }
}
