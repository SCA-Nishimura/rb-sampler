//! Decode audio files to interleaved stereo f32 at the engine sample rate.

use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct Sample {
    /// Interleaved stereo
    pub data: Vec<f32>,
}

impl Sample {
    pub fn frames(&self) -> usize {
        self.data.len() / 2
    }
}

pub fn load(path: &Path, target_rate: u32) -> Result<Sample, String> {
    let file = File::open(path).map_err(|e| format!("ファイルを開けません: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("対応していない形式です: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("音声トラックがありません")?;
    let track_id = track.id;
    let mut src_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("デコーダを作れません: {e}"))?;

    let mut stereo: Vec<f32> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("読み込みエラー: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("デコードエラー: {e}")),
        };
        let spec = *decoded.spec();
        src_rate = spec.rate;
        let ch = spec.channels.count().max(1);
        let sb = match &mut buf {
            Some(b) if b.capacity() >= decoded.capacity() * ch => b,
            _ => buf.insert(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)),
        };
        sb.copy_interleaved_ref(decoded);
        for frame in sb.samples().chunks_exact(ch) {
            let (l, r) = if ch == 1 { (frame[0], frame[0]) } else { (frame[0], frame[1]) };
            stereo.push(l);
            stereo.push(r);
        }
    }
    if stereo.is_empty() {
        return Err("音声データが空です".into());
    }
    Ok(Sample { data: resample_linear(&stereo, src_rate, target_rate) })
}

/// Simple linear-interpolation resampler (good enough for one-shot samples;
/// swap for a sinc resampler if quality becomes an issue).
fn resample_linear(input: &[f32], src: u32, dst: u32) -> Vec<f32> {
    if src == dst || src == 0 {
        return input.to_vec();
    }
    let in_frames = input.len() / 2;
    let ratio = src as f64 / dst as f64;
    let out_frames = ((in_frames as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let pos = i as f64 * ratio;
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(in_frames - 1);
        let t = (pos - i0 as f64) as f32;
        for c in 0..2 {
            let a = input[i0 * 2 + c];
            let b = input[i1 * 2 + c];
            out.push(a + (b - a) * t);
        }
    }
    out
}
