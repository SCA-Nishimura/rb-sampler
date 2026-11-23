//! Audio output engine (cpal). The stream lives on its own thread because
//! cpal::Stream is not Send. The realtime callback only talks to the rest of
//! the app through bounded channels (no locks, no allocation, no frees).

use crate::decode::Sample;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;

pub enum Cmd {
    Play { slot: usize, id: u64, sample: Arc<Sample>, gain: f32, looped: bool },
    Stop { slot: usize },
    StopAll,
    SetMaster(f32),
    SetGain { slot: usize, gain: f32 },
}

pub enum Event {
    /// Voice finished playing (naturally or after fade-out).
    Ended { slot: usize, id: u64 },
    /// Hand a sample back so it is freed outside the audio thread.
    Drop(#[allow(dead_code)] Arc<Sample>),
}

pub struct AudioInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
}

pub struct AudioEngine {
    pub info: AudioInfo,
    cmd_tx: Sender<Cmd>,
    _shutdown: Sender<()>,
}

impl AudioEngine {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmd_tx.try_send(cmd);
    }
}

pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Open an output stream. Returns the engine and the event receiver.
pub fn open(
    device_name: Option<&str>,
    output_pair: u16,
    buffer_frames: Option<u32>,
) -> Result<(AudioEngine, Receiver<Event>), String> {
    let (cmd_tx, cmd_rx) = bounded::<Cmd>(1024);
    let (ev_tx, ev_rx) = bounded::<Event>(1024);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(0);
    let (ready_tx, ready_rx) = bounded::<Result<AudioInfo, String>>(1);
    let device_name = device_name.map(|s| s.to_string());

    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let built = (|| -> Result<(cpal::Stream, AudioInfo), String> {
                let host = cpal::default_host();
                let device = match &device_name {
                    Some(name) => host
                        .output_devices()
                        .map_err(|e| e.to_string())?
                        .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                        .ok_or_else(|| format!("出力デバイスが見つかりません: {name}"))?,
                    None => host.default_output_device().ok_or("既定の出力デバイスがありません")?,
                };
                let supported = device.default_output_config().map_err(|e| e.to_string())?;
                let format = supported.sample_format();
                let mut config: cpal::StreamConfig = supported.config();
                let info = AudioInfo {
                    device: device.name().unwrap_or_default(),
                    sample_rate: config.sample_rate.0,
                    channels: config.channels,
                };
                let mixer = Mixer::new(cmd_rx, ev_tx, config.channels as usize, output_pair as usize, config.sample_rate.0);

                let build = |config: &cpal::StreamConfig, mixer: Mixer| -> Result<cpal::Stream, String> {
                    let err_fn = |e| eprintln!("audio stream error: {e}");
                    match format {
                        cpal::SampleFormat::F32 => build_stream::<f32>(&device, config, mixer, err_fn),
                        cpal::SampleFormat::I16 => build_stream::<i16>(&device, config, mixer, err_fn),
                        cpal::SampleFormat::I32 => build_stream::<i32>(&device, config, mixer, err_fn),
                        cpal::SampleFormat::U16 => build_stream::<u16>(&device, config, mixer, err_fn),
                        other => Err(format!("未対応のサンプル形式: {other:?}")),
                    }
                };

                let stream = if let Some(n) = buffer_frames {
                    config.buffer_size = cpal::BufferSize::Fixed(n);
                    match build(&config, mixer.clone_channels()) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("fixed buffer {n} failed ({e}), falling back to default");
                            config.buffer_size = cpal::BufferSize::Default;
                            build(&config, mixer)?
                        }
                    }
                } else {
                    build(&config, mixer)?
                };
                stream.play().map_err(|e| e.to_string())?;
                Ok((stream, info))
            })();

            match built {
                Ok((stream, info)) => {
                    let _ = ready_tx.send(Ok(info));
                    // Keep the stream alive until the engine is dropped.
                    let _ = shutdown_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let info = ready_rx.recv().map_err(|e| e.to_string())??;
    Ok((AudioEngine { info, cmd_tx, _shutdown: shutdown_tx }, ev_rx))
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut mixer: Mixer,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| mixer.render(out),
            err_fn,
            None,
        )
        .map_err(|e| e.to_string())
}

struct Voice {
    slot: usize,
    id: u64,
    sample: Arc<Sample>,
    pos: usize,
    gain: f32,
    looped: bool,
    /// Remaining fade-out frames when stopping.
    fade: Option<usize>,
}

const MAX_VOICES: usize = 64;

struct Mixer {
    cmd_rx: Receiver<Cmd>,
    ev_tx: Sender<Event>,
    channels: usize,
    pair: usize,
    voices: Vec<Voice>,
    master: f32,
    fade_len: usize,
}

impl Mixer {
    fn new(cmd_rx: Receiver<Cmd>, ev_tx: Sender<Event>, channels: usize, pair: usize, rate: u32) -> Self {
        let pair = if (pair * 2 + 1) < channels { pair } else { 0 };
        Self {
            cmd_rx,
            ev_tx,
            channels,
            pair,
            voices: Vec::with_capacity(MAX_VOICES),
            master: 1.0,
            fade_len: (rate / 200).max(1) as usize, // ~5ms
        }
    }

    /// Used only for the fixed-buffer retry: a second mixer sharing the same channels.
    fn clone_channels(&self) -> Self {
        Self {
            cmd_rx: self.cmd_rx.clone(),
            ev_tx: self.ev_tx.clone(),
            channels: self.channels,
            pair: self.pair,
            voices: Vec::with_capacity(MAX_VOICES),
            master: self.master,
            fade_len: self.fade_len,
        }
    }

    fn handle_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                Cmd::Play { slot, id, sample, gain, looped } => {
                    // one voice per slot: retrigger replaces
                    if let Some(i) = self.voices.iter().position(|v| v.slot == slot) {
                        let old = self.voices.swap_remove(i);
                        let _ = self.ev_tx.try_send(Event::Drop(old.sample));
                    }
                    if self.voices.len() >= MAX_VOICES {
                        let _ = self.ev_tx.try_send(Event::Drop(sample));
                        continue;
                    }
                    self.voices.push(Voice { slot, id, sample, pos: 0, gain, looped, fade: None });
                }
                Cmd::Stop { slot } => {
                    for v in self.voices.iter_mut().filter(|v| v.slot == slot && v.fade.is_none()) {
                        v.fade = Some(self.fade_len);
                    }
                }
                Cmd::StopAll => {
                    for v in self.voices.iter_mut().filter(|v| v.fade.is_none()) {
                        v.fade = Some(self.fade_len);
                    }
                }
                Cmd::SetMaster(g) => self.master = g,
                Cmd::SetGain { slot, gain } => {
                    for v in self.voices.iter_mut().filter(|v| v.slot == slot) {
                        v.gain = gain;
                    }
                }
            }
        }
    }

    fn render<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T]) {
        self.handle_commands();
        let ch = self.channels;
        let (li, ri) = if ch >= 2 { (self.pair * 2, self.pair * 2 + 1) } else { (0, 0) };
        let fade_len = self.fade_len as f32;

        for frame in out.chunks_mut(ch) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for v in self.voices.iter_mut() {
                let frames = v.sample.frames();
                if v.pos >= frames {
                    continue;
                }
                let mut g = v.gain;
                if let Some(f) = v.fade.as_mut() {
                    g *= *f as f32 / fade_len;
                    *f = f.saturating_sub(1);
                }
                l += v.sample.data[v.pos * 2] * g;
                r += v.sample.data[v.pos * 2 + 1] * g;
                v.pos += 1;
                if v.pos >= frames && v.looped && v.fade.is_none() {
                    v.pos = 0;
                }
            }
            l = (l * self.master).clamp(-1.0, 1.0);
            r = (r * self.master).clamp(-1.0, 1.0);
            for (i, s) in frame.iter_mut().enumerate() {
                let val = if ch == 1 {
                    (l + r) * 0.5
                } else if i == li {
                    l
                } else if i == ri {
                    r
                } else {
                    0.0
                };
                *s = T::from_sample(val);
            }
        }

        // reap finished voices
        let mut i = 0;
        while i < self.voices.len() {
            let v = &self.voices[i];
            if v.pos >= v.sample.frames() || v.fade == Some(0) {
                let v = self.voices.swap_remove(i);
                let _ = self.ev_tx.try_send(Event::Ended { slot: v.slot, id: v.id });
                let _ = self.ev_tx.try_send(Event::Drop(v.sample));
            } else {
                i += 1;
            }
        }
    }
}
