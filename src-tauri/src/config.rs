//! Persistent settings (saved as JSON in the app config dir).

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BANKS: usize = 4;
pub const PADS: usize = 16; // deck1 pads 1-8 + deck2 pads 1-8
pub const SLOTS: usize = BANKS * PADS;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlayMode {
    /// Press = play to the end (press again = restart).
    OneShot,
    /// Press = play / press again while playing = stop.
    Toggle,
    /// Plays only while held.
    Gate,
    /// Press = start loop / press again = stop.
    Loop,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PadConfig {
    pub path: String,
    pub name: String,
    pub mode: PlayMode,
    /// Linear gain 0.0..=2.0
    pub gain: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    Note,
    Cc,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Pad { index: u8 },
    Bank { index: u8 },
    StopAll,
    MasterGain,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub kind: MsgKind,
    /// MIDI channel 0-15 (displayed as 1-16)
    pub channel: u8,
    pub number: u8,
    pub action: Action,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub output_device: Option<String>,
    /// 0 = ch1/2, 1 = ch3/4, ...
    pub output_pair: u16,
    pub buffer_frames: Option<u32>,
    pub midi_input: Option<String>,
    pub midi_output: Option<String>,
    pub master_gain: f32,
    /// banks[bank][pad]
    pub banks: Vec<Vec<Option<PadConfig>>>,
    pub bindings: Vec<Binding>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output_device: None,
            output_pair: 0,
            buffer_frames: None,
            midi_input: None,
            midi_output: None,
            master_gain: 1.0,
            banks: vec![vec![None; PADS]; BANKS],
            bindings: default_bindings(),
        }
    }
}

/// Initial guess for DDJ-RB pads in SAMPLER mode.
/// Deck1: Note On ch8, notes 0x30-0x37 / Deck2: Note On ch10, notes 0x30-0x37.
/// Unverified on real hardware — fix via MIDI learn or by editing settings.json.
pub fn default_bindings() -> Vec<Binding> {
    let mut v = Vec::new();
    for i in 0..8u8 {
        v.push(Binding { kind: MsgKind::Note, channel: 7, number: 0x30 + i, action: Action::Pad { index: i } });
    }
    for i in 0..8u8 {
        v.push(Binding { kind: MsgKind::Note, channel: 9, number: 0x30 + i, action: Action::Pad { index: 8 + i } });
    }
    v
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s: Settings = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // normalize shape in case the file was edited by hand
        s.banks.resize(BANKS, vec![None; PADS]);
        for b in &mut s.banks {
            b.resize(PADS, None);
        }
        s.master_gain = s.master_gain.clamp(0.0, 2.0);
        s
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    pub fn pad(&self, slot: usize) -> Option<&PadConfig> {
        self.banks.get(slot / PADS)?.get(slot % PADS)?.as_ref()
    }
}
