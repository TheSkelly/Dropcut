//! Turning a transcript into caption cues on the *edited* timeline.

use serde::{Deserialize, Serialize};

use crate::profanity;
use crate::timeline::{map_span_in, Timeline};
use crate::transcript::Word;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CaptionSettings {
    /// Maximum characters on a caption (short = punchy social style).
    pub max_chars: usize,
    pub max_words: usize,
    /// Seconds a single caption may stay on screen.
    pub max_duration: f64,
    /// A pause longer than this always starts a new caption.
    pub break_on_gap: f64,
    pub uppercase: bool,
    /// Show censored words as "f***".
    pub mask_profanity: bool,
}

impl Default for CaptionSettings {
    fn default() -> Self {
        Self { max_chars: 32, max_words: 6, max_duration: 3.0, break_on_gap: 0.6, uppercase: false, mask_profanity: true }
    }
}

/// A word placed on the output timeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CueWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Original timing, used to break captions at removed pauses.
    #[serde(skip)]
    pub source: Option<(f64, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Per-word timing for animated (word-by-word) captions.
    pub words: Vec<CueWord>,
}

/// Words that survive the edit, moved to output time and masked if censored.
pub fn output_words(words: &[Word], timeline: &Timeline, settings: &CaptionSettings) -> Vec<CueWord> {
    let kept = timeline.kept();
    let censors = timeline.censors();
    words
        .iter()
        .filter_map(|w| {
            let span = map_span_in(&kept, w.span())?;
            // Words that were mostly cut (e.g. a deleted "um") are dropped.
            if span.len() < w.span().len() * 0.3 && w.span().len() > 0.0 {
                return None;
            }
            // Censor regions are padded, so require most of the word to be covered
            // (otherwise neighbouring words would get masked too).
            let covered: f64 = censors
                .iter()
                .map(|c| (c.end.min(w.end) - c.start.max(w.start)).max(0.0))
                .sum();
            let censored = covered >= w.span().len() * 0.6;
            let mut text = if censored && settings.mask_profanity { profanity::mask(&w.text) } else { w.text.clone() };
            if settings.uppercase {
                text = text.to_uppercase();
            }
            Some(CueWord { text, start: span.start, end: span.end, source: Some((w.start, w.end)) })
        })
        .collect()
}

pub fn build_cues(words: &[Word], timeline: &Timeline, settings: &CaptionSettings) -> Vec<Cue> {
    group(output_words(words, timeline, settings), settings)
}

fn ends_sentence(text: &str) -> bool {
    text.ends_with(['.', '!', '?'])
}

fn group(words: Vec<CueWord>, s: &CaptionSettings) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut current: Vec<CueWord> = Vec::new();

    let flush = |current: &mut Vec<CueWord>, cues: &mut Vec<Cue>| {
        if current.is_empty() {
            return;
        }
        let text = current.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        cues.push(Cue { start: current[0].start, end: current.last().unwrap().end, text, words: std::mem::take(current) });
    };

    for w in words {
        if let Some(last) = current.last() {
            let chars = current.iter().map(|w| w.text.len() + 1).sum::<usize>() + w.text.len();
            let too_long = chars > s.max_chars || current.len() >= s.max_words;
            let too_slow = w.end - current[0].start > s.max_duration;
            let gap = w.start - last.end > s.break_on_gap
                || matches!((last.source, w.source), (Some(a), Some(b)) if b.0 - a.1 > s.break_on_gap);
            if too_long || too_slow || gap || ends_sentence(&last.text) {
                flush(&mut current, &mut cues);
            }
        }
        current.push(w);
    }
    flush(&mut current, &mut cues);

    // Let each caption linger until the next one (up to a short limit) so
    // they don't flicker off between words.
    for i in 0..cues.len() {
        let next_start = cues.get(i + 1).map(|c| c.start).unwrap_or(f64::INFINITY);
        cues[i].end = cues[i].end.max((cues[i].end + 0.5).min(next_start));
    }
    cues
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

pub fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, srt_time(c.start), srt_time(c.end), c.text));
    }
    out
}

/// Convenience: SRT for the whole source with no edits applied.
pub fn span_cues_unedited(words: &[Word], duration: f64, settings: &CaptionSettings) -> Vec<Cue> {
    build_cues(words, &Timeline { duration, regions: Vec::new() }, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{RegionKind, Span};

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, confidence: 1.0 }
    }

    fn sample() -> Vec<Word> {
        vec![
            w("Hey", 0.0, 0.3),
            w("guys.", 0.3, 0.6),
            w("Today", 2.0, 2.3),
            w("we're", 2.3, 2.5),
            w("fucking", 2.5, 2.9),
            w("building", 2.9, 3.3),
            w("stuff.", 3.3, 3.7),
        ]
    }

    #[test]
    fn splits_on_sentences_and_shifts_for_cuts() {
        let mut t = Timeline::new(4.0);
        t.add(RegionKind::Silence, Span::new(0.8, 1.8), None);
        let s = CaptionSettings { max_chars: 40, ..Default::default() };
        let cues = build_cues(&sample(), &t, &s);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Hey guys.");
        assert_eq!(cues[1].text, "Today we're fucking building stuff.");
        assert!((cues[1].start - 1.0).abs() < 1e-9, "{}", cues[1].start);
    }

    #[test]
    fn masks_censored_words() {
        let mut t = Timeline::new(4.0);
        t.add(RegionKind::Profanity, Span::new(2.46, 2.94), Some("fucking".into()));
        let cues = build_cues(&sample(), &t, &CaptionSettings::default());
        assert!(cues.iter().any(|c| c.text.contains("f******")));
    }

    #[test]
    fn only_the_censored_word_is_masked() {
        let mut t = Timeline::new(4.0);
        // Padded region slightly overlapping the neighbours.
        t.add(RegionKind::Profanity, Span::new(2.46, 2.94), Some("fucking".into()));
        let words = output_words(&sample(), &t, &CaptionSettings::default());
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(&texts[3..6], ["we're", "f******", "building"]);
    }

    #[test]
    fn breaks_captions_where_a_pause_was_removed() {
        let words = vec![w("one", 0.0, 0.3), w("two", 0.3, 0.6), w("three", 3.0, 3.3)];
        let mut t = Timeline::new(4.0);
        t.add(RegionKind::Silence, Span::new(0.6, 3.0), None);
        let cues = build_cues(&words, &t, &CaptionSettings::default());
        assert_eq!(cues.len(), 2);
    }

    #[test]
    fn drops_words_that_were_cut() {
        let mut t = Timeline::new(4.0);
        t.add(RegionKind::Manual, Span::new(2.45, 2.95), None);
        let cues = build_cues(&sample(), &t, &CaptionSettings::default());
        assert!(cues.iter().all(|c| !c.text.contains("fucking")));
    }

    #[test]
    fn respects_word_limit() {
        let s = CaptionSettings { max_words: 2, ..Default::default() };
        let cues = span_cues_unedited(&sample(), 4.0, &s);
        assert!(cues.iter().all(|c| c.words.len() <= 2));
    }

    #[test]
    fn writes_srt() {
        let cues = span_cues_unedited(&sample()[..2], 1.0, &CaptionSettings::default());
        let srt = to_srt(&cues);
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,100\nHey guys.\n"), "{srt}");
        assert_eq!(srt_time(3661.5), "01:01:01,500");
    }
}
