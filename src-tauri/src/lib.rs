mod audio;
mod config;
mod decode;
mod midi;

use audio::{AudioEngine, Cmd, Event};
use config::{Action, Binding, MsgKind, PadConfig, PlayMode, Settings, BANKS, PADS, SLOTS};
use decode::Sample;
use midir::{MidiInputConnection, MidiOutputConnection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Core state
// ---------------------------------------------------------------------------

struct Core {
    app: AppHandle,
    me: Weak<Mutex<Core>>,
    settings_path: PathBuf,
    settings: Settings,
    bank: usize,

    audio: Option<AudioEngine>,
    audio_error: Option<String>,
    cache: HashMap<String, Arc<Sample>>,
    loading: HashSet<String>,
    load_errors: HashMap<String, String>,

    next_id: u64,
    voice_id: Vec<u64>,
    playing: Vec<bool>,
    /// Slot that each physical pad triggered on press (for Gate release after bank change).
    held: [Option<usize>; PADS],

    midi_out: Option<MidiOutputConnection>,
    midi_in_name: Option<String>,
    midi_out_name: Option<String>,
    midi_error: Option<String>,
    learn: Option<Action>,
    led_cache: [Option<u8>; PADS],
    blink: bool,
}

#[derive(Serialize, Clone)]
struct AudioStatus {
    device: String,
    sample_rate: u32,
    channels: u16,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
enum SlotStatus {
    Empty,
    Loading,
    Ready,
    Error,
}

#[derive(Serialize, Clone)]
struct Snapshot {
    settings: Settings,
    bank: usize,
    playing: Vec<bool>,
    slot_status: Vec<SlotStatus>,
    slot_errors: Vec<Option<String>>,
    audio: Option<AudioStatus>,
    audio_error: Option<String>,
    midi_input: Option<String>,
    midi_output: Option<String>,
    midi_error: Option<String>,
    learn: Option<Action>,
}

#[derive(Serialize, Clone)]
struct MidiLog {
    bytes: Vec<u8>,
    action: Option<String>,
}

#[derive(Serialize, Clone)]
struct PadState {
    slot: usize,
    playing: bool,
}

impl Core {
    fn save(&mut self) {
        if let Err(e) = self.settings.save(&self.settings_path) {
            eprintln!("failed to save settings: {e}");
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut slot_status = Vec::with_capacity(SLOTS);
        let mut slot_errors = Vec::with_capacity(SLOTS);
        for slot in 0..SLOTS {
            let (st, err) = match self.settings.pad(slot) {
                None => (SlotStatus::Empty, None),
                Some(p) if self.cache.contains_key(&p.path) => (SlotStatus::Ready, None),
                Some(p) if self.loading.contains(&p.path) => (SlotStatus::Loading, None),
                Some(p) => match self.load_errors.get(&p.path) {
                    Some(e) => (SlotStatus::Error, Some(e.clone())),
                    None => (SlotStatus::Loading, None),
                },
            };
            slot_status.push(st);
            slot_errors.push(err);
        }
        Snapshot {
            settings: self.settings.clone(),
            bank: self.bank,
            playing: self.playing.clone(),
            slot_status,
            slot_errors,
            audio: self.audio.as_ref().map(|a| AudioStatus {
                device: a.info.device.clone(),
                sample_rate: a.info.sample_rate,
                channels: a.info.channels,
            }),
            audio_error: self.audio_error.clone(),
            midi_input: self.midi_in_name.clone(),
            midi_output: self.midi_out_name.clone(),
            midi_error: self.midi_error.clone(),
            learn: self.learn,
        }
    }

    fn emit_state(&self) {
        let _ = self.app.emit("state", self.snapshot());
    }

    // ----- audio -----------------------------------------------------------

    fn open_audio(&mut self) {
        // stop old engine first
        self.audio = None;
        self.playing.iter_mut().for_each(|p| *p = false);
        self.voice_id.iter_mut().for_each(|v| *v = 0);

        let wanted = self.settings.output_device.clone().or_else(|| {
            // Auto-pick the DDJ if it's connected, otherwise the system default.
            audio::list_output_devices().into_iter().find(|n| n.to_uppercase().contains("DDJ"))
        });
        match audio::open(wanted.as_deref(), self.settings.output_pair, self.settings.buffer_frames) {
            Ok((engine, events)) => {
                engine.send(Cmd::SetMaster(self.settings.master_gain));
                self.audio = Some(engine);
                self.audio_error = None;
                // Sample rate may have changed: reload everything at the new rate.
                self.cache.clear();
                self.load_errors.clear();
                self.loading.clear();
                self.preload_all();
                let me = self.me.clone();
                std::thread::spawn(move || {
                    for ev in events {
                        match ev {
                            Event::Ended { slot, id } => {
                                if let Some(core) = me.upgrade() {
                                    core.lock().unwrap().on_ended(slot, id);
                                }
                            }
                            Event::Drop(s) => drop(s),
                        }
                    }
                });
            }
            Err(e) => {
                self.audio_error = Some(e);
            }
        }
        self.emit_state();
    }

    fn preload_all(&mut self) {
        let paths: Vec<String> = self
            .settings
            .banks
            .iter()
            .flatten()
            .flatten()
            .map(|p| p.path.clone())
            .collect();
        for p in paths {
            self.ensure_loaded(&p);
        }
    }

    fn ensure_loaded(&mut self, path: &str) {
        let Some(engine) = &self.audio else { return };
        if self.cache.contains_key(path) || self.loading.contains(path) {
            return;
        }
        self.loading.insert(path.to_string());
        self.load_errors.remove(path);
        let rate = engine.info.sample_rate;
        let me = self.me.clone();
        let path = path.to_string();
        std::thread::spawn(move || {
            let result = decode::load(Path::new(&path), rate);
            let Some(core) = me.upgrade() else { return };
            let mut c = core.lock().unwrap();
            // Ignore stale results (device was switched meanwhile).
            if !c.loading.remove(&path) || c.audio.as_ref().map(|a| a.info.sample_rate) != Some(rate) {
                return;
            }
            match result {
                Ok(s) => {
                    c.cache.insert(path, Arc::new(s));
                }
                Err(e) => {
                    c.load_errors.insert(path, e);
                }
            }
            c.led_cache = [None; PADS];
            c.refresh_leds();
            c.emit_state();
        });
    }

    // ----- playback --------------------------------------------------------

    fn press(&mut self, pad: usize) {
        if pad >= PADS {
            return;
        }
        let slot = self.bank * PADS + pad;
        self.held[pad] = Some(slot);
        let Some(cfg) = self.settings.pad(slot).cloned() else { return };
        match cfg.mode {
            PlayMode::OneShot | PlayMode::Gate => self.play(slot, &cfg, false),
            PlayMode::Toggle | PlayMode::Loop => {
                if self.playing[slot] {
                    self.stop(slot);
                } else {
                    self.play(slot, &cfg, cfg.mode == PlayMode::Loop);
                }
            }
        }
    }

    fn release(&mut self, pad: usize) {
        if pad >= PADS {
            return;
        }
        let Some(slot) = self.held[pad].take() else { return };
        if let Some(cfg) = self.settings.pad(slot) {
            if cfg.mode == PlayMode::Gate {
                self.stop(slot);
            }
        }
    }

    fn play(&mut self, slot: usize, cfg: &PadConfig, looped: bool) {
        let Some(sample) = self.cache.get(&cfg.path).cloned() else {
            self.ensure_loaded(&cfg.path);
            return;
        };
        let Some(engine) = &self.audio else { return };
        self.next_id += 1;
        let id = self.next_id;
        engine.send(Cmd::Play { slot, id, sample, gain: cfg.gain, looped });
        self.voice_id[slot] = id;
        self.set_playing(slot, true);
    }

    fn stop(&mut self, slot: usize) {
        if let Some(engine) = &self.audio {
            engine.send(Cmd::Stop { slot });
        }
    }

    fn stop_all(&mut self) {
        if let Some(engine) = &self.audio {
            engine.send(Cmd::StopAll);
        }
    }

    fn on_ended(&mut self, slot: usize, id: u64) {
        if self.voice_id.get(slot) == Some(&id) {
            self.voice_id[slot] = 0;
            self.set_playing(slot, false);
        }
    }

    fn set_playing(&mut self, slot: usize, playing: bool) {
        if self.playing[slot] != playing {
            self.playing[slot] = playing;
            let _ = self.app.emit("pad-state", PadState { slot, playing });
        }
        self.refresh_leds();
    }

    fn set_bank(&mut self, bank: usize) {
        if bank < BANKS && bank != self.bank {
            self.bank = bank;
            self.refresh_leds();
            self.emit_state();
        }
    }

    fn set_master(&mut self, gain: f32) {
        self.settings.master_gain = gain.clamp(0.0, 2.0);
        if let Some(engine) = &self.audio {
            engine.send(Cmd::SetMaster(self.settings.master_gain));
        }
    }

    // ----- MIDI ------------------------------------------------------------

    fn handle_midi(&mut self, bytes: &[u8]) {
        use midi::Msg;
        let msg = midi::parse(bytes);

        // MIDI learn
        if let Some(action) = self.learn {
            let learned = match msg {
                Msg::NoteOn { ch, note, .. } if action != Action::MasterGain => Some((MsgKind::Note, ch, note)),
                Msg::Cc { ch, num, .. } => Some((MsgKind::Cc, ch, num)),
                _ => None,
            };
            if let Some((kind, channel, number)) = learned {
                self.settings
                    .bindings
                    .retain(|b| b.action != action && !(b.kind == kind && b.channel == channel && b.number == number));
                self.settings.bindings.push(Binding { kind, channel, number, action });
                self.learn = None;
                self.led_cache = [None; PADS];
                self.save();
                self.refresh_leds();
                self.emit_state();
                let _ = self.app.emit("midi", MidiLog { bytes: bytes.to_vec(), action: Some("LEARN".into()) });
                return;
            }
        }

        let find = |kind: MsgKind, ch: u8, num: u8| {
            self.settings
                .bindings
                .iter()
                .find(|b| b.kind == kind && b.channel == ch && b.number == num)
                .map(|b| b.action)
        };
        let mut label = None;
        match msg {
            Msg::NoteOn { ch, note, .. } => {
                if let Some(a) = find(MsgKind::Note, ch, note) {
                    label = Some(format!("{a:?}"));
                    match a {
                        Action::Pad { index } => self.press(index as usize),
                        Action::Bank { index } => self.set_bank(index as usize),
                        Action::StopAll => self.stop_all(),
                        Action::MasterGain => {}
                    }
                }
            }
            Msg::NoteOff { ch, note } => {
                if let Some(a @ Action::Pad { index }) = find(MsgKind::Note, ch, note) {
                    label = Some(format!("{a:?} release"));
                    self.release(index as usize);
                }
            }
            Msg::Cc { ch, num, val } => {
                if let Some(a) = find(MsgKind::Cc, ch, num) {
                    label = Some(format!("{a:?}"));
                    match a {
                        Action::MasterGain => {
                            self.set_master(val as f32 / 127.0);
                            self.emit_state();
                        }
                        Action::Pad { index } if val > 0 => self.press(index as usize),
                        Action::Pad { index } => self.release(index as usize),
                        Action::Bank { index } if val > 0 => self.set_bank(index as usize),
                        Action::StopAll if val > 0 => self.stop_all(),
                        _ => {}
                    }
                }
            }
            Msg::Other => {}
        }
        let _ = self.app.emit("midi", MidiLog { bytes: bytes.to_vec(), action: label });
    }

    /// Pad LEDs: loaded = on, playing = blink, empty = off.
    fn refresh_leds(&mut self) {
        let Some(out) = self.midi_out.as_mut() else { return };
        for pad in 0..PADS {
            let slot = self.bank * PADS + pad;
            let loaded = self
                .settings
                .pad(slot)
                .map(|p| self.cache.contains_key(&p.path))
                .unwrap_or(false);
            let val = if self.playing[slot] {
                if self.blink { 0x7F } else { 0x00 }
            } else if loaded {
                0x7F
            } else {
                0x00
            };
            if self.led_cache[pad] == Some(val) {
                continue;
            }
            self.led_cache[pad] = Some(val);
            for b in &self.settings.bindings {
                if b.kind == MsgKind::Note && b.action == (Action::Pad { index: pad as u8 }) {
                    let _ = out.send(&[0x90 | b.channel, b.number, val]);
                }
            }
        }
    }

    fn leds_off(&mut self) {
        let Some(out) = self.midi_out.as_mut() else { return };
        for b in &self.settings.bindings {
            if b.kind == MsgKind::Note {
                let _ = out.send(&[0x90 | b.channel, b.number, 0]);
            }
        }
    }
}

struct AppState {
    core: Arc<Mutex<Core>>,
    /// Kept outside `core`: closing an input waits for the callback thread,
    /// which itself locks `core`.
    midi_in: Mutex<Option<MidiInputConnection<()>>>,
}

fn find_ddj(ports: Vec<String>) -> Option<String> {
    ports.into_iter().find(|n| n.to_uppercase().contains("DDJ"))
}

fn connect_midi(state: &AppState) {
    // Drop the old input without holding the core lock.
    let old = state.midi_in.lock().unwrap().take();
    drop(old);

    let (want_in, want_out) = {
        let mut c = state.core.lock().unwrap();
        c.leds_off();
        c.midi_out = None;
        c.midi_in_name = None;
        c.midi_out_name = None;
        c.midi_error = None;
        // None = auto-detect DDJ, Some("-") = disabled
        let pick = |s: &Option<String>, list: fn() -> Vec<String>| match s.as_deref() {
            Some("-") => None,
            Some(n) => Some(n.to_string()),
            None => find_ddj(list()),
        };
        (pick(&c.settings.midi_input, midi::list_inputs), pick(&c.settings.midi_output, midi::list_outputs))
    };

    let mut errors = Vec::new();
    if let Some(name) = &want_in {
        let weak = Arc::downgrade(&state.core);
        match midi::connect_input(name, move |bytes| {
            if let Some(core) = weak.upgrade() {
                core.lock().unwrap().handle_midi(bytes);
            }
        }) {
            Ok(conn) => {
                *state.midi_in.lock().unwrap() = Some(conn);
                state.core.lock().unwrap().midi_in_name = Some(name.clone());
            }
            Err(e) => errors.push(e),
        }
    }
    let mut c = state.core.lock().unwrap();
    if let Some(name) = &want_out {
        match midi::connect_output(name) {
            Ok(conn) => {
                c.midi_out = Some(conn);
                c.midi_out_name = Some(name.clone());
            }
            Err(e) => errors.push(e),
        }
    }
    c.midi_error = if errors.is_empty() { None } else { Some(errors.join(" / ")) };
    c.led_cache = [None; PADS];
    c.refresh_leds();
    c.emit_state();
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Devices {
    audio_outputs: Vec<String>,
    midi_inputs: Vec<String>,
    midi_outputs: Vec<String>,
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Snapshot {
    state.core.lock().unwrap().snapshot()
}

#[tauri::command]
fn list_devices() -> Devices {
    Devices {
        audio_outputs: audio::list_output_devices(),
        midi_inputs: midi::list_inputs(),
        midi_outputs: midi::list_outputs(),
    }
}

#[tauri::command]
fn set_audio(state: State<AppState>, device: Option<String>, output_pair: u16, buffer_frames: Option<u32>) {
    let mut c = state.core.lock().unwrap();
    c.settings.output_device = device;
    c.settings.output_pair = output_pair;
    c.settings.buffer_frames = buffer_frames;
    c.save();
    c.open_audio();
}

#[tauri::command]
fn set_midi(state: State<AppState>, input: Option<String>, output: Option<String>) {
    {
        let mut c = state.core.lock().unwrap();
        c.settings.midi_input = input;
        c.settings.midi_output = output;
        c.save();
    }
    connect_midi(&state);
}

#[tauri::command]
fn assign_pad(state: State<AppState>, bank: usize, pad: usize, path: String) -> Result<(), String> {
    if bank >= BANKS || pad >= PADS {
        return Err("invalid pad".into());
    }
    let mut c = state.core.lock().unwrap();
    let name = Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let prev = c.settings.banks[bank][pad].take();
    c.settings.banks[bank][pad] = Some(PadConfig {
        path: path.clone(),
        name,
        mode: prev.as_ref().map(|p| p.mode).unwrap_or(PlayMode::OneShot),
        gain: prev.as_ref().map(|p| p.gain).unwrap_or(1.0),
    });
    c.save();
    c.ensure_loaded(&path);
    c.led_cache = [None; PADS];
    c.refresh_leds();
    c.emit_state();
    Ok(())
}

#[tauri::command]
fn clear_pad(state: State<AppState>, bank: usize, pad: usize) {
    let mut c = state.core.lock().unwrap();
    if bank < BANKS && pad < PADS {
        c.stop(bank * PADS + pad);
        c.settings.banks[bank][pad] = None;
        c.save();
        c.led_cache = [None; PADS];
        c.refresh_leds();
        c.emit_state();
    }
}

#[tauri::command]
fn update_pad(state: State<AppState>, bank: usize, pad: usize, mode: PlayMode, gain: f32, name: String) {
    let mut c = state.core.lock().unwrap();
    if bank >= BANKS || pad >= PADS {
        return;
    }
    let gain = gain.clamp(0.0, 2.0);
    if let Some(p) = c.settings.banks[bank][pad].as_mut() {
        p.mode = mode;
        p.gain = gain;
        p.name = name;
    }
    if let Some(engine) = &c.audio {
        engine.send(Cmd::SetGain { slot: bank * PADS + pad, gain });
    }
    c.save();
    c.emit_state();
}

#[tauri::command]
fn pad_press(state: State<AppState>, pad: usize) {
    state.core.lock().unwrap().press(pad);
}

#[tauri::command]
fn pad_release(state: State<AppState>, pad: usize) {
    state.core.lock().unwrap().release(pad);
}

#[tauri::command]
fn set_bank(state: State<AppState>, bank: usize) {
    state.core.lock().unwrap().set_bank(bank);
}

#[tauri::command]
fn stop_all(state: State<AppState>) {
    state.core.lock().unwrap().stop_all();
}

#[tauri::command]
fn set_master(state: State<AppState>, gain: f32) {
    let mut c = state.core.lock().unwrap();
    c.set_master(gain);
    c.save();
}

#[tauri::command]
fn start_learn(state: State<AppState>, action: Action) {
    let mut c = state.core.lock().unwrap();
    c.learn = Some(action);
    c.emit_state();
}

#[tauri::command]
fn cancel_learn(state: State<AppState>) {
    let mut c = state.core.lock().unwrap();
    c.learn = None;
    c.emit_state();
}

#[tauri::command]
fn remove_binding(state: State<AppState>, index: usize) {
    let mut c = state.core.lock().unwrap();
    if index < c.settings.bindings.len() {
        c.leds_off();
        c.settings.bindings.remove(index);
        c.save();
        c.led_cache = [None; PADS];
        c.refresh_leds();
        c.emit_state();
    }
}

#[tauri::command]
fn reset_bindings(state: State<AppState>) {
    let mut c = state.core.lock().unwrap();
    c.leds_off();
    c.settings.bindings = config::default_bindings();
    c.save();
    c.led_cache = [None; PADS];
    c.refresh_leds();
    c.emit_state();
}

#[tauri::command]
fn settings_path(state: State<AppState>) -> String {
    state.core.lock().unwrap().settings_path.display().to_string()
}

// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            let settings_path = dir.join("settings.json");
            let settings = Settings::load(&settings_path);
            let handle = app.handle().clone();
            let core = Arc::new_cyclic(|me| {
                Mutex::new(Core {
                    app: handle,
                    me: me.clone(),
                    settings_path,
                    settings,
                    bank: 0,
                    audio: None,
                    audio_error: None,
                    cache: HashMap::new(),
                    loading: HashSet::new(),
                    load_errors: HashMap::new(),
                    next_id: 0,
                    voice_id: vec![0; SLOTS],
                    playing: vec![false; SLOTS],
                    held: [None; PADS],
                    midi_out: None,
                    midi_in_name: None,
                    midi_out_name: None,
                    midi_error: None,
                    learn: None,
                    led_cache: [None; PADS],
                    blink: false,
                })
            });
            core.lock().unwrap().open_audio();
            let state = AppState { core: core.clone(), midi_in: Mutex::new(None) };
            connect_midi(&state);
            app.manage(state);

            // LED blink ticker
            let weak = Arc::downgrade(&core);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let Some(core) = weak.upgrade() else { break };
                let mut c = core.lock().unwrap();
                c.blink = !c.blink;
                let bank = c.bank;
                if c.playing[bank * PADS..(bank + 1) * PADS].iter().any(|p| *p) {
                    c.refresh_leds();
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window.try_state::<AppState>() {
                    state.core.lock().unwrap().leds_off();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            list_devices,
            set_audio,
            set_midi,
            assign_pad,
            clear_pad,
            update_pad,
            pad_press,
            pad_release,
            set_bank,
            stop_all,
            set_master,
            start_learn,
            cancel_learn,
            remove_binding,
            reset_bindings,
            settings_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
