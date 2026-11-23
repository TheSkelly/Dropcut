//! Silence detection on decoded audio.
//!
//! The approach is deliberately simple and predictable: measure loudness in
//! 10 ms frames, mark frames under a threshold as quiet, and turn runs of
//! quiet frames that are long enough into removable spans, keeping a little
//! padding on each side so speech never gets clipped.

use serde::{Deserialize, Serialize};

use crate::timeline::Span;

const FRAME_SECS: f64 = 0.010;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SilenceSettings {
    /// Frames quieter than this (dBFS) count as silence. `None` = automatic,
    /// which adapts to the recording's own noise floor.
    pub threshold_db: Option<f32>,
    /// Only remove pauses at least this long (seconds).
    pub min_silence: f64,
    /// Audio kept before speech resumes (seconds).
    pub pad_before: f64,
    /// Audio kept after speech stops (seconds).
    pub pad_after: f64,
}

impl Default for SilenceSettings {
    fn default() -> Self {
        Self { threshold_db: None, min_silence: 0.5, pad_before: 0.12, pad_after: 0.18 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SilenceAnalysis {
    /// The threshold that was actually used (dBFS).
    pub threshold_db: f32,
    /// Spans that can be removed, already padded.
    pub silences: Vec<Span>,
}

/// Loudness of each 10 ms frame in dBFS (floored at -100).
pub fn frame_levels_db(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    let frame = ((sample_rate as f64 * FRAME_SECS) as usize).max(1);
    samples
        .chunks(frame)
        .map(|chunk| {
            let mean_sq = chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32;
            (10.0 * mean_sq.max(1e-10).log10()).max(-100.0)
        })
        .collect()
}

/// Pick a threshold from the recording itself: a bit above the noise floor,
/// but never so high that it eats quiet speech.
pub fn auto_threshold(levels: &[f32]) -> f32 {
    if levels.is_empty() {
        return -40.0;
    }
    let mut sorted: Vec<f32> = levels.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
    let floor = pct(0.10);
    let speech = pct(0.90);
    // 30% of the way from the noise floor to typical speech loudness.
    let t = floor + (speech - floor) * 0.30;
    t.clamp(-60.0, -20.0)
}

pub fn detect(samples: &[f32], sample_rate: u32, settings: &SilenceSettings) -> SilenceAnalysis {
    let levels = frame_levels_db(samples, sample_rate);
    detect_from_levels(&levels, settings)
}

pub fn detect_from_levels(levels: &[f32], settings: &SilenceSettings) -> SilenceAnalysis {
    let threshold_db = settings.threshold_db.unwrap_or_else(|| auto_threshold(levels));
    let total = levels.len() as f64 * FRAME_SECS;
    let mut silences = Vec::new();

    let mut i = 0;
    while i < levels.len() {
        if levels[i] >= threshold_db {
            i += 1;
            continue;
        }
        let start_frame = i;
        while i < levels.len() && levels[i] < threshold_db {
            i += 1;
        }
        let raw_start = start_frame as f64 * FRAME_SECS;
        let raw_end = i as f64 * FRAME_SECS;
        if raw_end - raw_start < settings.min_silence {
            continue;
        }
        // Leading/trailing silence in the file needs no padding on the outer edge.
        let start = if start_frame == 0 { 0.0 } else { raw_start + settings.pad_after };
        let end = if i >= levels.len() { total } else { raw_end - settings.pad_before };
        if end - start >= 0.05 {
            silences.push(Span::new(start, end));
        }
    }

    SilenceAnalysis { threshold_db, silences }
}

/// Where someone is (probably) talking: the gaps between unpadded silences
/// of at least `min_gap` seconds. Used to tighten word timings.
pub fn speech_spans(levels: &[f32], threshold_db: f32, min_gap: f64) -> Vec<Span> {
    let settings = SilenceSettings { threshold_db: Some(threshold_db), min_silence: min_gap, pad_before: 0.0, pad_after: 0.0 };
    let silences = detect_from_levels(levels, &settings).silences;
    let total = levels.len() as f64 * FRAME_SECS;
    let mut out = Vec::new();
    let mut cursor = 0.0;
    for s in silences {
        if s.start > cursor {
            out.push(Span::new(cursor, s.start));
        }
        cursor = s.end;
    }
    if cursor < total {
        out.push(Span::new(cursor, total));
    }
    out
}

/// Min/max peaks for drawing a waveform, `buckets` pairs long.
pub fn waveform_peaks(samples: &[f32], buckets: usize) -> Vec<[f32; 2]> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let per = samples.len().div_ceil(buckets);
    samples
        .chunks(per)
        .map(|c| {
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for &s in c {
                lo = lo.min(s);
                hi = hi.max(s);
            }
            [lo, hi]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(secs: f64) -> Vec<f32> {
        (0..(secs * RATE as f64) as usize)
            .map(|i| 0.3 * (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin())
            .collect()
    }

    fn quiet(secs: f64) -> Vec<f32> {
        vec![0.0005; (secs * RATE as f64) as usize]
    }

    #[test]
    fn finds_gap_between_speech() {
        let audio = [tone(1.0), quiet(1.0), tone(1.0)].concat();
        let s = SilenceSettings { threshold_db: Some(-40.0), pad_before: 0.1, pad_after: 0.1, ..Default::default() };
        let r = detect(&audio, RATE, &s);
        assert_eq!(r.silences.len(), 1);
        let gap = r.silences[0];
        assert!((gap.start - 1.1).abs() < 0.02, "{gap:?}");
        assert!((gap.end - 1.9).abs() < 0.02, "{gap:?}");
    }

    #[test]
    fn ignores_short_pauses() {
        let audio = [tone(1.0), quiet(0.3), tone(1.0)].concat();
        let r = detect(&audio, RATE, &SilenceSettings { threshold_db: Some(-40.0), ..Default::default() });
        assert!(r.silences.is_empty());
    }

    #[test]
    fn trims_leading_and_trailing_silence_fully() {
        let audio = [quiet(1.0), tone(1.0), quiet(1.0)].concat();
        let r = detect(&audio, RATE, &SilenceSettings { threshold_db: Some(-40.0), ..Default::default() });
        assert_eq!(r.silences.len(), 2);
        assert_eq!(r.silences[0].start, 0.0);
        assert!((r.silences[1].end - 3.0).abs() < 0.02);
    }

    #[test]
    fn auto_threshold_sits_between_floor_and_speech() {
        let audio = [tone(2.0), quiet(2.0), tone(2.0)].concat();
        let levels = frame_levels_db(&audio, RATE);
        let t = auto_threshold(&levels);
        assert!(t > -66.0 && t < -13.0, "{t}");
        let r = detect_from_levels(&levels, &SilenceSettings::default());
        assert_eq!(r.silences.len(), 1);
    }

    #[test]
    fn peaks_have_requested_length() {
        let p = waveform_peaks(&tone(1.0), 100);
        assert_eq!(p.len(), 100);
        assert!(p[0][1] > 0.2 && p[0][0] < -0.2);
    }
}
