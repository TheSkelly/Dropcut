//! Word-level transcripts.
//!
//! Captions, profanity filtering and (later) bad-take removal, chapters and
//! viral clip picking all work from the same list of timed words, which comes
//! from whisper.cpp running locally (GPU accelerated where available).

use serde::{Deserialize, Serialize};

use crate::timeline::Span;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    /// The word as spoken, including any attached punctuation ("okay,").
    pub text: String,
    /// Source time, seconds.
    pub start: f64,
    pub end: f64,
    /// Model confidence 0..1, handy for highlighting words worth checking.
    #[serde(default = "one")]
    pub confidence: f32,
}

fn one() -> f32 {
    1.0
}

impl Word {
    pub fn span(&self) -> Span {
        Span::new(self.start, self.end)
    }

    /// Lowercased letters/digits only, for matching ("F*ck!" -> "fck").
    pub fn normalized(&self) -> String {
        normalize(&self.text)
    }
}

pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .filter(|c| *c != '\'')
        .collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    pub language: Option<String>,
    pub words: Vec<Word>,
}

impl Transcript {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TranscribeOptions {
    /// ISO code such as "en", or `None` to auto-detect.
    pub language: Option<String>,
    pub use_gpu: bool,
    pub threads: usize,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
        Self { language: None, use_gpu: true, threads }
    }
}

/// Raw token as produced by the recogniser, before being joined into words.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
pub(crate) struct RawToken {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub p: f32,
}

/// Join sub-word tokens into words. Whisper's BPE tokens that start with a
/// space begin a new word; anything else (including punctuation) is glued
/// onto the previous one.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
pub(crate) fn tokens_to_words(tokens: impl IntoIterator<Item = RawToken>) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut probs: Vec<(f32, usize)> = Vec::new();
    for t in tokens {
        if t.text.is_empty() {
            continue;
        }
        let starts_word = t.text.starts_with(' ') || words.is_empty();
        let trimmed = t.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if starts_word {
            words.push(Word { text: trimmed.to_string(), start: t.start, end: t.end, confidence: t.p });
            probs.push((t.p, 1));
        } else if let (Some(w), Some(p)) = (words.last_mut(), probs.last_mut()) {
            w.text.push_str(trimmed);
            w.end = w.end.max(t.end);
            p.0 += t.p;
            p.1 += 1;
        }
    }
    for (w, (sum, n)) in words.iter_mut().zip(probs) {
        w.confidence = sum / n as f32;
        if w.end < w.start {
            w.end = w.start;
        }
    }
    words
}

/// Fit word timings onto detected speech so no word sits in a pause.
///
/// `speech` is sorted, non-overlapping spans where audio is above the
/// silence threshold. Each word is assigned (in order) to the stretch of
/// speech it overlaps most or is nearest to; within a stretch, the
/// recogniser's timings are kept where plausible and nudged apart where
/// they collide, so every word gets a sensible minimum length.
pub fn align_to_speech(words: &mut [Word], speech: &[Span]) {
    if speech.is_empty() || words.is_empty() {
        return;
    }

    // 1. Assign each word to a speech span, never going backwards.
    let mut assigned: Vec<usize> = Vec::with_capacity(words.len());
    let mut min_idx = 0;
    for (k, w) in words.iter().enumerate() {
        let span = w.span();
        let mid = (span.start + span.end) / 2.0;
        let in_pause = !speech[min_idx..].iter().any(|s| s.overlaps(&span));
        if in_pause && k > 0 {
            // A word stranded in a pause belongs to the sentence it finishes
            // ("...to the channel.") or the one it starts ("Today ...").
            let prev_ends_sentence = ends_sentence(&words[k - 1].text);
            let next_span = (min_idx + 1).min(speech.len() - 1);
            let choice = if ends_sentence(&w.text) || !prev_ends_sentence {
                // Continue the previous word's sentence unless it's clearly far away.
                let near_prev = distance(&speech[min_idx], span.start);
                let near_next = distance(&speech[next_span], span.end);
                if near_prev <= near_next * 3.0 { min_idx } else { next_span }
            } else {
                next_span
            };
            assigned.push(choice);
            min_idx = choice;
            continue;
        }
        let best = (min_idx..speech.len())
            .max_by(|&a, &b| {
                let score = |i: usize| {
                    let s = &speech[i];
                    // Overlap counts most; otherwise prefer the closest span.
                    overlap(s, &span) * 10.0 - distance(s, mid)
                };
                score(a).total_cmp(&score(b))
            })
            .unwrap_or(min_idx);
        assigned.push(best);
        min_idx = best;
    }

    // 2. Fit the words of each span inside it.
    let mut i = 0;
    while i < words.len() {
        let idx = assigned[i];
        let mut j = i;
        while j < words.len() && assigned[j] == idx {
            j += 1;
        }
        fit_run(&mut words[i..j], speech[idx]);
        i = j;
    }
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', ')']).ends_with(['.', '!', '?'])
}

/// Minimum believable duration of a spoken word.
fn min_len(w: &Word) -> f64 {
    0.06 + 0.035 * w.text.chars().filter(|c| c.is_alphanumeric()).count() as f64
}

fn fit_run(run: &mut [Word], span: Span) {
    let n = run.len();
    let mut need: Vec<f64> = run.iter().map(min_len).collect();
    let total: f64 = need.iter().sum();
    if total > span.len() {
        // Fast talker or short span: shrink minimums to fit.
        let k = span.len() / total;
        need.iter_mut().for_each(|d| *d *= k);
    }

    let mut starts: Vec<f64> = run.iter().map(|w| w.start.clamp(span.start, span.end)).collect();
    // Forward: each word starts after the previous one has had its minimum time.
    for k in 0..n {
        let floor = if k == 0 { span.start } else { starts[k - 1] + need[k - 1] };
        starts[k] = starts[k].max(floor);
    }
    // Backward: everything (including the last word's minimum) fits before span end.
    for k in (0..n).rev() {
        let ceil = if k + 1 == n { span.end - need[k] } else { starts[k + 1] - need[k] };
        starts[k] = starts[k].min(ceil).max(span.start);
    }

    for k in 0..n {
        let next = if k + 1 < n { starts[k + 1] } else { span.end };
        let orig_end = run[k].end.clamp(starts[k] + need[k], span.end);
        run[k].start = starts[k];
        // End at the next word, unless the recogniser saw a pause before it.
        run[k].end = if k + 1 < n { next.min(orig_end.max(next - 0.25)) } else { orig_end.min(starts[k] + 1.5) };
        if run[k].end < run[k].start + need[k] {
            run[k].end = (run[k].start + need[k]).min(span.end);
        }
    }
}

fn overlap(a: &Span, b: &Span) -> f64 {
    (a.end.min(b.end) - a.start.max(b.start)).max(0.0)
}

fn distance(s: &Span, t: f64) -> f64 {
    if s.contains(t) {
        0.0
    } else {
        (s.start - t).abs().min((s.end - t).abs())
    }
}

#[cfg(feature = "whisper")]
pub use whisper_impl::transcribe;

#[cfg(feature = "whisper")]
mod whisper_impl {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use whisper_rs::{
        DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
    };

    use super::*;
    use crate::{Error, Result};

    /// Transcribe 16 kHz mono samples. `progress` receives 0..=100.
    pub fn transcribe(
        model_path: &Path,
        samples: &[f32],
        options: &TranscribeOptions,
        progress: impl FnMut(i32) + 'static,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<Transcript> {
        if !model_path.is_file() {
            return Err(Error::NotFound(model_path.to_path_buf()));
        }
        let mut ctx_params = WhisperContextParameters::default();
        ctx_params.use_gpu(options.use_gpu);
        // DTW alignment gives far tighter word timings than Whisper's default
        // token timestamps, which matters for captions and word-level cuts.
        if let Some(preset) = dtw_preset(model_path) {
            ctx_params.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: preset }, ..Default::default() });
        }
        let ctx = WhisperContext::new_with_params(model_path, ctx_params)
            .map_err(|e| Error::Transcribe(format!("could not load model: {e}")))?;
        let mut state = ctx.create_state().map_err(|e| Error::Transcribe(e.to_string()))?;

        let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
        params.set_n_threads(options.threads as i32);
        params.set_language(Some(options.language.as_deref().unwrap_or("auto")));
        params.set_token_timestamps(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_progress_callback_safe(progress);
        if let Some(flag) = cancel.clone() {
            params.set_abort_callback_safe(move || flag.load(Ordering::Relaxed));
        }

        state.full(params, samples).map_err(|e| {
            if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                Error::Cancelled
            } else {
                Error::Transcribe(e.to_string())
            }
        })?;

        let eot = ctx.token_eot();
        let mut tokens = Vec::new();
        for segment in state.as_iter() {
            for i in 0..segment.n_tokens() {
                let Some(token) = segment.get_token(i) else { continue };
                // Special tokens (timestamps, start/end markers) all sit at or above EOT.
                if token.token_id() >= eot {
                    continue;
                }
                let data = token.token_data();
                let Ok(text) = token.to_str_lossy() else { continue };
                let dtw = (data.t_dtw >= 0).then(|| data.t_dtw as f64 / 100.0);
                tokens.push(RawToken {
                    text: text.into_owned(),
                    start: dtw.unwrap_or(data.t0 as f64 / 100.0),
                    end: data.t1 as f64 / 100.0,
                    p: data.p,
                });
            }
        }

        let has_dtw = dtw_preset(model_path).is_some();
        let lang_id = state.full_lang_id_from_state();
        let language = whisper_rs::get_lang_str(lang_id).map(str::to_string);
        let mut words = tokens_to_words(tokens);
        if has_dtw {
            // DTW gives reliable onsets; end each word where the next begins.
            for i in 0..words.len() {
                let next = words.get(i + 1).map(|w| w.start);
                let w = &mut words[i];
                w.end = match next {
                    Some(n) if n > w.start => n.min(w.start + 1.5),
                    _ => w.end.max(w.start + 0.2),
                };
            }
        }
        Ok(Transcript { language, words })
    }

    fn dtw_preset(model_path: &Path) -> Option<DtwModelPreset> {
        let name = model_path.file_name()?.to_string_lossy().to_lowercase();
        let id = name.strip_prefix("ggml-")?.split(['.', '-']).next()?.to_string();
        let rest = name.strip_prefix("ggml-")?;
        let en = rest.contains(".en");
        Some(if rest.starts_with("large-v3-turbo") {
            DtwModelPreset::LargeV3Turbo
        } else if rest.starts_with("large-v3") {
            DtwModelPreset::LargeV3
        } else if rest.starts_with("large-v2") {
            DtwModelPreset::LargeV2
        } else {
            match (id.as_str(), en) {
                ("tiny", true) => DtwModelPreset::TinyEn,
                ("tiny", false) => DtwModelPreset::Tiny,
                ("base", true) => DtwModelPreset::BaseEn,
                ("base", false) => DtwModelPreset::Base,
                ("small", true) => DtwModelPreset::SmallEn,
                ("small", false) => DtwModelPreset::Small,
                ("medium", true) => DtwModelPreset::MediumEn,
                ("medium", false) => DtwModelPreset::Medium,
                _ => return None,
            }
        })
    }
}

/// Stub so callers compile without the `whisper` feature.
#[cfg(not(feature = "whisper"))]
pub fn transcribe(
    _model_path: &std::path::Path,
    _samples: &[f32],
    _options: &TranscribeOptions,
    _progress: impl FnMut(i32) + 'static,
    _cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> crate::Result<Transcript> {
    Err(crate::Error::WhisperUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, start: f64, end: f64) -> RawToken {
        RawToken { text: text.into(), start, end, p: 0.9 }
    }

    #[test]
    fn joins_subword_tokens() {
        let words = tokens_to_words([
            tok(" Hello", 0.0, 0.4),
            tok(",", 0.4, 0.45),
            tok(" unbe", 0.5, 0.7),
            tok("lievable", 0.7, 1.1),
            tok("!", 1.1, 1.12),
        ]);
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Hello,", "unbelievable!"]);
        assert_eq!(words[1].start, 0.5);
        assert_eq!(words[1].end, 1.12);
    }

    #[test]
    fn first_token_without_space_starts_a_word() {
        let words = tokens_to_words([tok("So", 0.0, 0.2), tok(" yeah", 0.2, 0.5)]);
        assert_eq!(words.len(), 2);
    }

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, confidence: 1.0 }
    }

    #[test]
    fn moves_words_out_of_pauses_in_order() {
        // Speech at 1.5-4.5 and 7.5-10.5; the recogniser drifted into the pause.
        let speech = [Span::new(1.5, 4.5), Span::new(7.5, 10.5)];
        let mut words = vec![
            word("welcome", 1.7, 3.2),
            word("back", 3.3, 4.8),
            word("to", 5.1, 5.3),
            word("the", 5.1, 5.6),
            word("channel.", 5.1, 6.6),
            word("Today", 7.8, 8.0),
        ];
        align_to_speech(&mut words, &speech);
        for w in &words[..5] {
            assert!(w.start >= 1.5 && w.end <= 4.5 + 1e-9, "{w:?}");
        }
        for pair in words.windows(2) {
            assert!(pair[1].start >= pair[0].start + 0.05, "{pair:?}");
            assert!(pair[0].end <= pair[1].start + 1e-9 || pair[0].end <= 4.5, "{pair:?}");
        }
        assert!(words[5].start >= 7.5);
    }

    #[test]
    fn keeps_good_timings() {
        let speech = [Span::new(0.0, 2.0)];
        let mut words = vec![word("hello", 0.1, 0.5), word("there", 0.6, 1.0)];
        align_to_speech(&mut words, &speech);
        assert!((words[0].start - 0.1).abs() < 1e-9);
        assert!((words[1].start - 0.6).abs() < 1e-9);
    }

    #[test]
    fn normalizes_for_matching() {
        assert_eq!(normalize("Don't!"), "dont");
        assert_eq!(normalize("F*CK"), "fck");
    }
}
