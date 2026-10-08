//! Streaming decode to interleaved `f32` with symphonia 0.6.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub extension: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub bits_per_sample: Option<u32>,
    pub frames: u64,
}

impl StreamInfo {
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }
}

/// Decode `path`, handing every decoded packet to `sink` as interleaved samples.
/// `sink` receives `(info_so_far, interleaved_samples)`; the rate and channel count are
/// fixed from the first packet onward.
pub fn decode_file(path: &Path, mut sink: impl FnMut(&StreamInfo, &[f32])) -> Result<StreamInfo> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !extension.is_empty() {
        hint.with_extension(&extension);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("unsupported or unreadable audio file ({e})"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("no audio track in {}", path.display()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("no decodable audio track in {}", path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow!("no decoder for this audio codec ({e})"))?;

    let mut info = StreamInfo {
        extension,
        codec: decoder.codec_info().long_name.to_string(),
        sample_rate: params.sample_rate.unwrap_or(0),
        channels: params.channels.as_ref().map_or(0, |c| c.count()),
        bits_per_sample: params.bits_per_sample.or(params.bits_per_coded_sample),
        frames: 0,
    };
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(anyhow!("reading audio failed: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                let spec = audio.spec();
                info.sample_rate = spec.rate();
                info.channels = spec.channels().count();
                buf.resize(audio.samples_interleaved(), 0.0);
                audio.copy_to_slice_interleaved(&mut buf);
                info.frames += audio.frames() as u64;
                sink(&info, &buf);
            }
            // A corrupt packet is skipped; the rest of the file is still analysable.
            Err(Error::DecodeError(_)) => {}
            Err(e) => return Err(anyhow!("decoding audio failed: {e}")),
        }
    }
    if info.frames == 0 || info.sample_rate == 0 || info.channels == 0 {
        bail!("no audio could be decoded from {}", path.display());
    }
    Ok(info)
}
