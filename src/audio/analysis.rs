//! Loudness, peak/RMS, tempo (spectral flux + autocorrelation) and key
//! (chromagram + Krumhansl-Schmuckler) estimates. Tempo and key are estimates, not facts.

use std::f32::consts::PI;
use std::path::Path;

use anyhow::{Result, anyhow};
use ebur128::{EbuR128, Mode};
use realfft::RealFftPlanner;

use super::decode::{StreamInfo, decode_file};

/// Mono analysis signal is decimated to roughly this rate.
const ANALYSIS_RATE: u32 = 11_025;
/// Tempo and key look at the first ten minutes only.
const MAX_ANALYSIS_SECS: f64 = 600.0;

#[derive(Debug, Clone)]
pub struct TempoEstimate {
    pub bpm: f64,
    /// Normalized autocorrelation at the chosen lag, 0..1.
    pub strength: f64,
    /// Other plausible readings: (bpm, why).
    pub alternatives: Vec<(f64, &'static str)>,
}

#[derive(Debug, Clone)]
pub struct KeyEstimate {
    pub name: String,
    pub correlation: f64,
    pub alternatives: Vec<(String, f64)>,
}

#[derive(Debug, Clone)]
pub struct Analysis {
    pub info: StreamInfo,
    pub peak_db: f64,
    pub rms_db: f64,
    pub lufs: Option<f64>,
    pub lra: Option<f64>,
    pub tempo: Option<TempoEstimate>,
    pub key: Option<KeyEstimate>,
    pub analysed_secs: f64,
}

fn to_db(x: f64) -> f64 {
    if x <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * x.log10()
    }
}

pub fn analyze_file(path: &Path) -> Result<Analysis> {
    let mut peak = 0f32;
    let mut sum_sq = 0f64;
    let mut count = 0u64;
    let mut meter: Option<EbuR128> = None;
    let mut meter_failed = false;
    let mut mono: Vec<f32> = Vec::new();
    let mut decim = 1usize;
    let mut acc = 0f32;
    let mut acc_n = 0usize;
    let mut rate_seen = 0u32;

    let info = decode_file(path, |info, samples| {
        if rate_seen != info.sample_rate {
            rate_seen = info.sample_rate;
            decim = (info.sample_rate / ANALYSIS_RATE).max(1) as usize;
        }
        for &s in samples {
            peak = peak.max(s.abs());
            sum_sq += (s as f64) * (s as f64);
        }
        count += samples.len() as u64;
        if meter.is_none() && !meter_failed {
            match EbuR128::new(
                info.channels as u32,
                info.sample_rate,
                Mode::I | Mode::LRA | Mode::SAMPLE_PEAK,
            ) {
                Ok(m) => meter = Some(m),
                Err(_) => meter_failed = true,
            }
        }
        if let Some(m) = meter.as_mut() {
            let _ = m.add_frames_f32(samples);
        }
        let ch = info.channels.max(1);
        let max_len = (MAX_ANALYSIS_SECS * info.sample_rate as f64 / decim as f64) as usize;
        if mono.len() < max_len {
            for frame in samples.chunks_exact(ch) {
                acc += frame.iter().sum::<f32>() / ch as f32;
                acc_n += 1;
                if acc_n == decim {
                    mono.push(acc / decim as f32);
                    acc = 0.0;
                    acc_n = 0;
                }
            }
        }
    })?;

    let rms = if count == 0 {
        0.0
    } else {
        (sum_sq / count as f64).sqrt()
    };
    let lufs = meter
        .as_ref()
        .and_then(|m| m.loudness_global().ok())
        .filter(|v| v.is_finite());
    let lra = meter
        .as_ref()
        .and_then(|m| m.loudness_range().ok())
        .filter(|v| v.is_finite());
    let analysis_rate = info.sample_rate as f32 / decim as f32;
    let silent = peak < 1e-5;
    Ok(Analysis {
        peak_db: to_db(peak as f64),
        rms_db: to_db(rms),
        lufs,
        lra,
        tempo: if silent {
            None
        } else {
            estimate_tempo(&mono, analysis_rate)
        },
        key: if silent {
            None
        } else {
            estimate_key(&mono, analysis_rate)
        },
        analysed_secs: mono.len() as f64 / analysis_rate as f64,
        info,
    })
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
        .collect()
}

/// Log-magnitude spectral flux per frame, mean-subtracted and half-wave rectified.
fn onset_envelope(mono: &[f32], win: usize, hop: usize) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(win);
    let window = hann(win);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let mut prev = vec![0f32; win / 2 + 1];
    let mut env = Vec::new();
    let mut start = 0;
    while start + win <= mono.len() {
        for (i, w) in window.iter().enumerate() {
            input[i] = mono[start + i] * w;
        }
        if fft.process(&mut input, &mut output).is_err() {
            break;
        }
        let mut flux = 0f32;
        for (k, c) in output.iter().enumerate().skip(2) {
            let mag = (1.0 + 100.0 * c.norm() / win as f32).ln();
            flux += (mag - prev[k]).max(0.0);
            prev[k] = mag;
        }
        env.push(flux);
        start += hop;
    }
    // Subtract a local mean (~0.5 s) so steady loudness doesn't dominate the autocorrelation.
    let half = 22usize;
    let mut out = Vec::with_capacity(env.len());
    let mut prefix = vec![0f64];
    for v in &env {
        prefix.push(prefix.last().unwrap() + *v as f64);
    }
    for i in 0..env.len() {
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(env.len());
        let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
        out.push((env[i] as f64 - mean).max(0.0) as f32);
    }
    out
}

pub fn estimate_tempo(mono: &[f32], rate: f32) -> Option<TempoEstimate> {
    const WIN: usize = 1024;
    const HOP: usize = 128;
    const MIN_BPM: f64 = 40.0;
    const MAX_BPM: f64 = 240.0;
    if (mono.len() as f32) < rate * 4.0 {
        return None;
    }
    let env = onset_envelope(mono, WIN, HOP);
    let fps = rate as f64 / HOP as f64;
    let min_lag = (fps * 60.0 / MAX_BPM).floor() as usize;
    let max_lag = (fps * 60.0 / MIN_BPM).ceil() as usize;
    if env.len() < max_lag * 3 {
        return None;
    }
    let n = env.len();
    let mean = env.iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let centered: Vec<f64> = env.iter().map(|v| *v as f64 - mean).collect();
    let r0: f64 = centered.iter().map(|v| v * v).sum::<f64>() / n as f64;
    if r0 <= 1e-12 {
        return None;
    }
    let acf: Vec<f64> = (0..=2 * max_lag)
        .map(|lag| {
            let s: f64 = (0..n - lag).map(|i| centered[i] * centered[i + lag]).sum();
            s / (n - lag) as f64 / r0
        })
        .collect();
    let bpm_at = |lag: f64| 60.0 * fps / lag;
    let prior = |bpm: f64| (-0.5 * (bpm / 120.0).log2().powi(2)).exp();
    let score = |lag: usize| {
        let mut s = acf[lag].max(0.0);
        if 2 * lag < acf.len() {
            s += 0.5 * acf[2 * lag].max(0.0);
        }
        s * prior(bpm_at(lag as f64))
    };
    let mut best = min_lag.max(1);
    for lag in min_lag.max(1)..=max_lag {
        if score(lag) > score(best) {
            best = lag;
        }
    }
    // Parabolic interpolation around the peak for sub-frame lag.
    let refined = if best > 1 && best + 1 < acf.len() {
        let (a, b, c) = (acf[best - 1], acf[best], acf[best + 1]);
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-12 {
            best as f64 + 0.5 * (a - c) / denom
        } else {
            best as f64
        }
    } else {
        best as f64
    };
    let bpm = bpm_at(refined.clamp(best as f64 - 1.0, best as f64 + 1.0));
    let mut alternatives = Vec::new();
    if bpm / 2.0 >= MIN_BPM {
        alternatives.push((bpm / 2.0, "half-time"));
    }
    if bpm * 2.0 <= MAX_BPM * 1.25 {
        alternatives.push((bpm * 2.0, "double-time"));
    }
    Some(TempoEstimate {
        bpm,
        strength: acf[best].clamp(0.0, 1.0),
        alternatives,
    })
}

const MAJOR_PROFILE: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];
const MAJOR_NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];
const MINOR_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "G#", "A", "Bb", "B",
];

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        num += (x - ma) * (y - mb);
        da += (x - ma).powi(2);
        db += (y - mb).powi(2);
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

pub fn chromagram(mono: &[f32], rate: f32) -> [f64; 12] {
    const WIN: usize = 4096;
    const HOP: usize = 2048;
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WIN);
    let window = hann(WIN);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let bin_hz = rate as f64 / WIN as f64;
    let lo = (100.0 / bin_hz).ceil() as usize;
    let hi = ((2000.0 / bin_hz) as usize).min(WIN / 2);
    let mut chroma = [0f64; 12];
    let mut start = 0;
    while start + WIN <= mono.len() {
        for (i, w) in window.iter().enumerate() {
            input[i] = mono[start + i] * w;
        }
        if fft.process(&mut input, &mut output).is_err() {
            break;
        }
        for (k, c) in output.iter().enumerate().take(hi + 1).skip(lo) {
            let hz = k as f64 * bin_hz;
            let midi = 69.0 + 12.0 * (hz / 440.0).log2();
            let pc = (midi.round() as i64).rem_euclid(12) as usize;
            chroma[pc] += c.norm() as f64;
        }
        start += HOP;
    }
    chroma
}

pub fn estimate_key(mono: &[f32], rate: f32) -> Option<KeyEstimate> {
    if (mono.len() as f32) < rate * 3.0 {
        return None;
    }
    let chroma = chromagram(mono, rate);
    if chroma.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    let mut scored: Vec<(String, f64)> = Vec::with_capacity(24);
    for tonic in 0..12 {
        for (profile, names, suffix) in [
            (&MAJOR_PROFILE, &MAJOR_NAMES, "major"),
            (&MINOR_PROFILE, &MINOR_NAMES, "minor"),
        ] {
            let rotated: Vec<f64> = (0..12).map(|i| profile[(i + 12 - tonic) % 12]).collect();
            scored.push((
                format!("{} {suffix}", names[tonic]),
                pearson(&chroma, &rotated),
            ));
        }
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (name, correlation) = scored[0].clone();
    Some(KeyEstimate {
        name,
        correlation,
        alternatives: scored[1..4].to_vec(),
    })
}

fn channel_label(n: usize) -> String {
    match n {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n} channels"),
    }
}

fn fmt_db(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.1}")
    } else {
        "-inf".into()
    }
}

/// Human-readable report, written for the model to relay. Tempo and key are labelled estimates.
pub fn report(path: &Path, a: &Analysis) -> String {
    let i = &a.info;
    let secs = i.duration_secs();
    let mut out = format!(
        "File: {}\nFormat: {} ({})\nDuration: {}:{:04.1} ({secs:.1} s), {} Hz, {}{}\n",
        path.display(),
        if i.extension.is_empty() {
            "unknown"
        } else {
            &i.extension
        },
        i.codec,
        (secs / 60.0).floor() as u64,
        secs % 60.0,
        i.sample_rate,
        channel_label(i.channels),
        i.bits_per_sample
            .map_or(String::new(), |b| format!(", {b}-bit")),
    );
    out.push_str(&format!(
        "Peak: {} dBFS, RMS: {} dBFS\n",
        fmt_db(a.peak_db),
        fmt_db(a.rms_db)
    ));
    match (a.lufs, a.lra) {
        (Some(l), Some(r)) => out.push_str(&format!(
            "Loudness: {l:.1} LUFS integrated, loudness range {r:.1} LU\n"
        )),
        (Some(l), None) => out.push_str(&format!("Loudness: {l:.1} LUFS integrated\n")),
        _ => out.push_str("Loudness: not measurable (silence or too short)\n"),
    }
    if a.peak_db.is_infinite() || a.peak_db < -100.0 {
        out.push_str("The file is silent; tempo and key were not estimated.");
        return out;
    }
    match &a.tempo {
        Some(t) => {
            let conf = if t.strength > 0.5 {
                "high"
            } else if t.strength > 0.25 {
                "medium"
            } else {
                "low"
            };
            let alts: Vec<String> = t
                .alternatives
                .iter()
                .map(|(b, why)| format!("{b:.1} ({why})"))
                .collect();
            out.push_str(&format!(
                "Tempo (estimate): {:.1} BPM, {conf} confidence; also plausible: {}\n",
                t.bpm,
                alts.join(", ")
            ));
        }
        None => out.push_str("Tempo (estimate): not enough rhythmic material to estimate\n"),
    }
    match &a.key {
        Some(k) => {
            let alts: Vec<String> = k
                .alternatives
                .iter()
                .map(|(n, r)| format!("{n} ({r:.2})"))
                .collect();
            let conf = if k.correlation > 0.75 {
                "high"
            } else if k.correlation > 0.55 {
                "medium"
            } else {
                "low"
            };
            out.push_str(&format!(
                "Key (estimate): {} (correlation {:.2}, {conf} confidence); alternatives: {}\n",
                k.name,
                k.correlation,
                alts.join(", ")
            ));
        }
        None => out.push_str("Key (estimate): not enough tonal material to estimate\n"),
    }
    if a.analysed_secs + 1.0 < secs {
        out.push_str(&format!(
            "Tempo and key come from the first {:.0} s only.\n",
            a.analysed_secs
        ));
    }
    out.push_str("Tempo and key are signal-analysis estimates; confirm by ear.");
    out
}

/// Convenience wrapper for the tool: errors carry context for the model.
pub fn analyze_and_report(path: &Path) -> Result<String> {
    let a = analyze_file(path).map_err(|e| anyhow!("{e:#}"))?;
    Ok(report(path, &a))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    fn write_wav(path: &Path, channels: u16, samples: &[f32]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: SR,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .unwrap();
        }
        w.finalize().unwrap();
    }

    /// Decaying 1 kHz plus broadband-ish clicks on every beat.
    fn click_track(bpm: f64, secs: f64) -> Vec<f32> {
        let n = (secs * SR as f64) as usize;
        let mut out = vec![0f32; n];
        let beat = 60.0 / bpm * SR as f64;
        let mut t = 0.0;
        let mut seed = 12345u32;
        while (t as usize) < n {
            for j in 0..1500 {
                let i = t as usize + j;
                if i >= n {
                    break;
                }
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 8) as f32 / (1 << 23) as f32 - 1.0;
                let env = (-(j as f32) / 300.0).exp();
                out[i] += 0.6
                    * env
                    * (0.6 * noise + 0.4 * (2.0 * PI * 1000.0 * j as f32 / SR as f32).sin());
            }
            t += beat;
        }
        out
    }

    fn chord_progression(chords: &[&[f32]], secs_each: f64) -> Vec<f32> {
        let mut out = Vec::new();
        for freqs in chords {
            let n = (secs_each * SR as f64) as usize;
            for i in 0..n {
                let t = i as f32 / SR as f32;
                let mut s = 0.0;
                for f in freqs.iter() {
                    for h in 1..=4 {
                        s += (2.0 * PI * f * h as f32 * t).sin() / h as f32;
                    }
                }
                out.push(0.1 * s);
            }
        }
        out
    }

    fn tmp_wav(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("sf-audio-{name}-{}.wav", uuid::Uuid::new_v4()))
    }

    #[test]
    fn tempo_of_click_tracks() {
        for bpm in [90.0, 120.0, 140.0] {
            let path = tmp_wav("tempo");
            write_wav(&path, 1, &click_track(bpm, 24.0));
            let a = analyze_file(&path).unwrap();
            let t = a.tempo.expect("tempo");
            assert!(
                (t.bpm - bpm).abs() / bpm < 0.02,
                "wanted {bpm}, got {}",
                t.bpm
            );
            assert!(t.alternatives.iter().any(|(_, w)| *w == "half-time"));
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn key_of_chord_progressions() {
        // C F G C
        let c = [130.81, 164.81, 196.0];
        let f = [174.61, 220.0, 261.63];
        let g = [196.0, 246.94, 293.66];
        let path = tmp_wav("keyc");
        write_wav(&path, 1, &chord_progression(&[&c, &f, &g, &c], 3.0));
        let k = analyze_file(&path).unwrap().key.expect("key");
        assert_eq!(k.name, "C major", "{k:?}");
        std::fs::remove_file(path).ok();
        // Am Dm E Am
        let am = [110.0, 130.81, 164.81];
        let dm = [146.83, 174.61, 220.0];
        let e = [164.81, 207.65, 246.94];
        let path = tmp_wav("keya");
        write_wav(&path, 1, &chord_progression(&[&am, &dm, &e, &am], 3.0));
        let k = analyze_file(&path).unwrap().key.expect("key");
        assert_eq!(k.name, "A minor", "{k:?}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn levels_loudness_and_format() {
        // 5 s of a 997 Hz sine at -20 dBFS peak, stereo.
        let n = 5 * SR as usize;
        let amp = 0.1f32;
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = amp * (2.0 * PI * 997.0 * i as f32 / SR as f32).sin();
            samples.push(s);
            samples.push(s);
        }
        let path = tmp_wav("levels");
        write_wav(&path, 2, &samples);
        let a = analyze_file(&path).unwrap();
        assert_eq!(a.info.sample_rate, SR);
        assert_eq!(a.info.channels, 2);
        assert!((a.info.duration_secs() - 5.0).abs() < 0.01);
        assert!((a.peak_db + 20.0).abs() < 0.2, "{}", a.peak_db);
        // RMS of a sine is 3 dB below its peak.
        assert!((a.rms_db + 23.0).abs() < 0.3, "{}", a.rms_db);
        // A 997 Hz sine at -23 dBFS RMS per channel, two channels summed: about -20 LUFS.
        let lufs = a.lufs.expect("lufs");
        assert!((lufs + 20.0).abs() < 1.0, "{lufs}");
        let text = report(&path, &a);
        assert!(text.contains("44100 Hz, stereo, 16-bit"), "{text}");
        assert!(text.contains("estimates"), "{text}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn silence_and_garbage() {
        let path = tmp_wav("silence");
        write_wav(&path, 1, &vec![0.0; SR as usize * 2]);
        let a = analyze_file(&path).unwrap();
        assert!(a.tempo.is_none() && a.key.is_none());
        assert!(report(&path, &a).contains("silent"));
        std::fs::remove_file(&path).ok();

        let bad = std::env::temp_dir().join(format!("sf-audio-bad-{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&bad, b"definitely not audio").unwrap();
        assert!(analyze_file(&bad).is_err());
        std::fs::remove_file(bad).ok();
        assert!(analyze_file(Path::new("/no/such/file.wav")).is_err());
    }
}
