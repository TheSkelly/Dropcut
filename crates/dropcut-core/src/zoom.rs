//! Automatic punch-in zooms.
//!
//! Talking-head edits look jumpy when the same framing is cut together, so
//! editors alternate between a wide and a punched-in framing. DropCut does
//! that automatically, on the *edited* timeline:
//!
//! * `OnCuts`: switch framing at every cut DropCut made.
//! * `OnSentences`: switch at sentence starts, at most every `min_hold` seconds
//!   (works even for footage with few pauses).
//!
//! Zooms are instant (like a camera switch), which also avoids the sub-pixel
//! wobble of slow animated zooms.

use serde::{Deserialize, Serialize};

use crate::captions::CueWord;
use crate::timeline::Timeline;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ZoomMode {
    #[default]
    OnCuts,
    OnSentences,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ZoomSettings {
    pub enabled: bool,
    pub mode: ZoomMode,
    /// Punched-in scale, e.g. 1.15 = 15% closer.
    pub amount: f64,
    /// Never switch framing faster than this (seconds).
    pub min_hold: f64,
    /// Vertical focus point, 0 = top, 1 = bottom. Faces sit around 0.4.
    pub focus_y: f64,
}

impl Default for ZoomSettings {
    fn default() -> Self {
        Self { enabled: false, mode: ZoomMode::OnCuts, amount: 1.15, min_hold: 2.0, focus_y: 0.4 }
    }
}

/// A stretch of the output during which the picture is punched in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ZoomSegment {
    pub start: f64,
    pub end: f64,
    pub scale: f64,
}

/// Output times where framing may change, before applying `min_hold`.
fn switch_points(timeline: &Timeline, words: &[CueWord], mode: ZoomMode) -> Vec<f64> {
    match mode {
        ZoomMode::OnCuts => {
            let mut t = 0.0;
            let mut out = Vec::new();
            for s in timeline.kept() {
                if t > 0.0 {
                    out.push(t);
                }
                t += s.len();
            }
            out
        }
        ZoomMode::OnSentences => words
            .windows(2)
            .filter(|p| p[0].text.trim_end_matches(['"', '\'', ')']).ends_with(['.', '!', '?']) || p[1].start - p[0].end > 0.6)
            .map(|p| p[1].start)
            .collect(),
    }
}

/// Plan punch-ins for the edited video. `words` are output-time words (from
/// [`crate::captions::output_words`]) and are only needed for `OnSentences`.
pub fn plan(timeline: &Timeline, words: &[CueWord], settings: &ZoomSettings) -> Vec<ZoomSegment> {
    if !settings.enabled || settings.amount <= 1.0 {
        return Vec::new();
    }
    let total = timeline.output_duration();
    let mut points = switch_points(timeline, words, settings.mode);
    points.sort_by(|a, b| a.total_cmp(b));

    // Enforce a minimum hold so framing never flickers.
    let mut kept_points = Vec::new();
    let mut last = 0.0;
    for p in points {
        if p - last >= settings.min_hold && total - p >= settings.min_hold * 0.5 {
            kept_points.push(p);
            last = p;
        }
    }

    // Alternate wide / punched-in, starting wide.
    let mut bounds = vec![0.0];
    bounds.extend(kept_points);
    bounds.push(total);
    bounds
        .windows(2)
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, w)| ZoomSegment { start: w[0], end: w[1], scale: settings.amount })
        .filter(|z| z.end > z.start)
        .collect()
}

/// The zoompan filter for `segments`, keeping the output at `width`x`height`.
/// Time variable `it` is the frame's output timestamp (zoom runs after cuts).
pub fn zoompan_filter(segments: &[ZoomSegment], width: u32, height: u32, fps: f64, focus_y: f64) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    let z = segments
        .iter()
        .map(|s| format!("{:.4}*between(it\\,{:.3}\\,{:.3})", s.scale - 1.0, s.start, s.end - 0.001))
        .collect::<Vec<_>>()
        .join("+");
    let fy = focus_y.clamp(0.0, 1.0);
    // zoompan needs even output dimensions.
    let (w, h) = (width & !1, height & !1);
    Some(format!(
        "zoompan=z='1+{z}':x='iw/2-iw/zoom/2':y='max(0\\,min(ih-ih/zoom\\,ih*{fy:.3}-ih/zoom/2))':d=1:s={w}x{h}:fps={fps:.3}"
    ))
}

/// Zoom scale at output time `t` (for the UI preview).
pub fn scale_at(segments: &[ZoomSegment], t: f64) -> f64 {
    segments.iter().find(|s| t >= s.start && t < s.end).map_or(1.0, |s| s.scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{RegionKind, Span};

    fn w(text: &str, start: f64) -> CueWord {
        CueWord { text: text.into(), start, end: start + 0.3, source: None }
    }

    #[test]
    fn alternates_on_cuts() {
        let mut t = Timeline::new(20.0);
        for (a, b) in [(3.0, 4.0), (7.0, 8.0), (11.0, 12.0)] {
            t.add(RegionKind::Silence, Span::new(a, b), None);
        }
        // Output: 0-3 | 3-6 | 6-9 | 9-17
        let z = plan(&t, &[], &ZoomSettings { enabled: true, ..Default::default() });
        assert_eq!(z.len(), 2);
        assert_eq!((z[0].start, z[0].end), (3.0, 6.0));
        assert_eq!((z[1].start, z[1].end), (9.0, 17.0));
    }

    #[test]
    fn respects_min_hold() {
        let mut t = Timeline::new(10.0);
        for (a, b) in [(1.0, 1.1), (1.5, 1.6), (5.0, 5.1)] {
            t.add(RegionKind::Silence, Span::new(a, b), None);
        }
        let z = plan(&t, &[], &ZoomSettings { enabled: true, min_hold: 2.0, ..Default::default() });
        // Cuts at output ~0.9 and ~1.3 are too soon; ~4.7 is the first switch.
        assert_eq!(z.len(), 1);
        assert!((z[0].start - 4.8).abs() < 0.05, "{z:?}");
    }

    #[test]
    fn sentences_mode_uses_punctuation() {
        let t = Timeline::new(12.0);
        let words = [w("Hi.", 0.0), w("This", 3.0), w("is", 3.4), w("cool!", 3.8), w("Next", 7.0), w("one", 7.4)];
        let z = plan(&t, &words, &ZoomSettings { enabled: true, mode: ZoomMode::OnSentences, ..Default::default() });
        assert_eq!(z.len(), 1);
        assert_eq!((z[0].start, z[0].end), (3.0, 7.0));
    }

    #[test]
    fn disabled_means_no_zoom() {
        let t = Timeline::new(10.0);
        assert!(plan(&t, &[], &ZoomSettings::default()).is_empty());
    }

    #[test]
    fn builds_filter() {
        let f = zoompan_filter(&[ZoomSegment { start: 1.0, end: 2.0, scale: 1.2 }], 1921, 1080, 30.0, 0.4).unwrap();
        assert!(f.starts_with("zoompan=z='1+0.2000*between(it\\,1.000\\,1.999)'"), "{f}");
        assert!(f.contains("s=1920x1080"));
        assert_eq!(scale_at(&[ZoomSegment { start: 1.0, end: 2.0, scale: 1.2 }], 1.5), 1.2);
    }
}
