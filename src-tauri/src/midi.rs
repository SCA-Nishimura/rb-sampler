//! MIDI port helpers (midir).

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

pub fn list_inputs() -> Vec<String> {
    let Ok(mi) = MidiInput::new("rb-sampler-list") else { return vec![] };
    mi.ports().iter().filter_map(|p| mi.port_name(p).ok()).collect()
}

pub fn list_outputs() -> Vec<String> {
    let Ok(mo) = MidiOutput::new("rb-sampler-list") else { return vec![] };
    mo.ports().iter().filter_map(|p| mo.port_name(p).ok()).collect()
}

pub fn connect_input<F>(name: &str, mut on_msg: F) -> Result<MidiInputConnection<()>, String>
where
    F: FnMut(&[u8]) + Send + 'static,
{
    let mut mi = MidiInput::new("rb-sampler-in").map_err(|e| e.to_string())?;
    mi.ignore(midir::Ignore::TimeAndActiveSense);
    let port = mi
        .ports()
        .into_iter()
        .find(|p| mi.port_name(p).ok().as_deref() == Some(name))
        .ok_or_else(|| format!("MIDI入力が見つかりません: {name}"))?;
    mi.connect(&port, "rb-sampler-in", move |_ts, msg, _| on_msg(msg), ())
        .map_err(|e| format!("MIDI入力を開けません（他のアプリ（rekordbox等）が使用中の可能性）: {e}"))
}

pub fn connect_output(name: &str) -> Result<MidiOutputConnection, String> {
    let mo = MidiOutput::new("rb-sampler-out").map_err(|e| e.to_string())?;
    let port = mo
        .ports()
        .into_iter()
        .find(|p| mo.port_name(p).ok().as_deref() == Some(name))
        .ok_or_else(|| format!("MIDI出力が見つかりません: {name}"))?;
    mo.connect(&port, "rb-sampler-out")
        .map_err(|e| format!("MIDI出力を開けません: {e}"))
}

/// Parsed channel message relevant to us.
#[derive(Clone, Copy, Debug)]
pub enum Msg {
    NoteOn { ch: u8, note: u8 },
    NoteOff { ch: u8, note: u8 },
    Cc { ch: u8, num: u8, val: u8 },
    Other,
}

pub fn parse(bytes: &[u8]) -> Msg {
    if bytes.len() < 3 {
        return Msg::Other;
    }
    let ch = bytes[0] & 0x0F;
    match bytes[0] & 0xF0 {
        0x90 if bytes[2] > 0 => Msg::NoteOn { ch, note: bytes[1] },
        0x90 | 0x80 => Msg::NoteOff { ch, note: bytes[1] },
        0xB0 => Msg::Cc { ch, num: bytes[1], val: bytes[2] },
        _ => Msg::Other,
    }
}
