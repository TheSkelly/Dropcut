//! Short-form clip finder: picks the best 20 to 75 second moments from a long
//! video's transcript and frames them for vertical (9:16) platforms.
//!
//! Two selectors share one output type:
//!
//! * [`find_heuristic`] works offline from the transcript alone (speech
//!   density, hooks, questions, clean sentence boundaries).
//! * [`find_with_llm`] (feature `download`) asks Claude to pick moments, then
//!   the answer is snapped to sentence edges and validated by the same code
//!   that guards the heuristic, so a bad model reply can never produce a clip
//!   that cuts mid-sentence, overlaps another, or leaves the video.

use serde::{Deserialize, Serialize};

use crate::transcript::Word;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShortsSettings {
    /// How many shorts to return at most.
    pub count: usize,
    pub min_seconds: f64,
    pub max_seconds: f64,
}

impl Default for ShortsSettings {
    fn default() -> Self {
        Self { count: 5, min_seconds: 20.0, max_seconds: 75.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShortCandidate {
    /// Source time, seconds.
    pub start: f64,
    pub end: f64,
    pub title: String,
    /// Opening line, shown in the UI so the user can judge the hook.
    pub hook: String,
    /// 0..=100, higher is better.
    pub score: u32,
    /// `"llm"` or `"heuristic"`.
    pub source: String,
}

impl ShortCandidate {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// A run of words ending at sentence punctuation or a long pause.
#[derive(Debug, Clone)]
struct Sentence {
    start: f64,
    end: f64,
    first: usize,
    last: usize, // inclusive word index
    text: String,
}

const GAP_BREAK: f64 = 0.8;

fn sentences(words: &[Word]) -> Vec<Sentence> {
    let mut out = Vec::new();
    let mut first = 0usize;
    for i in 0..words.len() {
        let terminal = words[i].text.ends_with(['.', '?', '!', '…']);
        let gap = words.get(i + 1).map(|n| n.start - words[i].end > GAP_BREAK).unwrap_or(true);
        if terminal || gap || i + 1 == words.len() {
            let text = words[first..=i].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            out.push(Sentence { start: words[first].start, end: words[i].end, first, last: i, text });
            first = i + 1;
        }
    }
    out
}

fn title_from(text: &str) -> String {
    let t: String = text.split_whitespace().take(7).collect::<Vec<_>>().join(" ");
    t.trim_matches(|c: char| !c.is_alphanumeric()).to_string()
}

const HOOK_WORDS: &[&str] = &[
    "secret", "mistake", "never", "always", "why", "how", "biggest", "stop", "truth", "nobody", "everyone",
    "actually", "problem", "worst", "best", "free", "money", "learn", "wrong", "story",
];

fn count_hooks(text: &str) -> usize {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| HOOK_WORDS.contains(&w.as_str()))
        .count()
}

/// Score one candidate window made of `sents[i..=j]`.
fn score_window(words: &[Word], sents: &[Sentence], i: usize, j: usize) -> u32 {
    let (a, b) = (&sents[i], &sents[j]);
    let dur = (b.end - a.start).max(0.001);
    let n_words = b.last - a.first + 1;
    let spoken: f64 = words[a.first..=b.last].iter().map(|w| (w.end - w.start).max(0.0)).sum();

    // Dense speech holds attention; ~2.5 to 3.5 words/second is ideal.
    let wps = n_words as f64 / dur;
    let density = (1.0 - ((wps - 3.0).abs() / 3.0)).clamp(0.0, 1.0);
    // Dead air inside the window is boring.
    let talk_ratio = (spoken / dur).clamp(0.0, 1.0);

    let hook_text = &a.text;
    let mut hook = 0.0;
    if hook_text.contains('?') {
        hook += 0.5;
    }
    hook += 0.25 * count_hooks(hook_text).min(2) as f64;
    if hook_text.chars().any(|c| c.is_ascii_digit()) {
        hook += 0.15;
    }
    let hook = hook.min(1.0);

    let body: String = sents[i..=j].iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    let energy = ((body.matches('!').count() + body.matches('?').count()) as f64 / (dur / 15.0)).min(1.0);
    let payoff = if b.text.ends_with(['.', '!', '?']) { 1.0 } else { 0.3 };

    let s = 0.30 * density + 0.20 * talk_ratio + 0.25 * hook + 0.10 * energy + 0.15 * payoff;
    (s * 100.0).round().clamp(0.0, 100.0) as u32
}

fn overlaps(a: &ShortCandidate, b: &ShortCandidate) -> bool {
    a.start < b.end && b.start < a.end
}

/// Highest-scoring first, dropping overlaps, then back in time order.
fn pick_top(mut cands: Vec<ShortCandidate>, count: usize) -> Vec<ShortCandidate> {
    cands.sort_by(|a, b| b.score.cmp(&a.score).then(a.start.total_cmp(&b.start)));
    let mut chosen: Vec<ShortCandidate> = Vec::new();
    for c in cands {
        if chosen.len() >= count {
            break;
        }
        if !chosen.iter().any(|k| overlaps(k, &c)) {
            chosen.push(c);
        }
    }
    chosen.sort_by(|a, b| a.start.total_cmp(&b.start));
    chosen
}

/// Offline selection from the transcript alone.
pub fn find_heuristic(words: &[Word], settings: &ShortsSettings) -> Vec<ShortCandidate> {
    if words.is_empty() {
        return Vec::new();
    }
    let sents = sentences(words);
    let mut cands = Vec::new();
    for i in 0..sents.len() {
        // Best end for this start: longest-valid windows are compared by score.
        let mut best: Option<(u32, usize)> = None;
        for j in i..sents.len() {
            let dur = sents[j].end - sents[i].start;
            if dur > settings.max_seconds {
                break;
            }
            if dur < settings.min_seconds {
                continue;
            }
            let s = score_window(words, &sents, i, j);
            if best.is_none_or(|(bs, _)| s > bs) {
                best = Some((s, j));
            }
        }
        if let Some((score, j)) = best {
            cands.push(ShortCandidate {
                start: sents[i].start,
                end: sents[j].end,
                title: title_from(&sents[i].text),
                hook: sents[i].text.clone(),
                score,
                source: "heuristic".into(),
            });
        }
    }
    pick_top(cands, settings.count)
}

// ------------------------------------------------------------------ LLM path

/// Transcript with sentence timestamps, one line each, for the model.
pub fn format_transcript(words: &[Word]) -> String {
    sentences(words)
        .iter()
        .map(|s| format!("[{:.1}-{:.1}] {}", s.start, s.end, s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn build_prompt(words: &[Word], settings: &ShortsSettings) -> String {
    format!(
        "You are an expert short-form video editor (TikTok, Reels, Shorts).\n\
         Below is a timestamped transcript of a long video. Pick the {n} best self-contained moments \
         to turn into clips. Each clip must:\n\
         - run between {lo} and {hi} seconds\n\
         - start on a strong hook and end on a complete thought (never mid-sentence)\n\
         - not overlap other clips\n\
         Return ONLY a JSON array, no prose, best first:\n\
         [{{\"start\": <seconds>, \"end\": <seconds>, \"title\": \"<short title>\", \
         \"hook\": \"<opening line>\", \"score\": <0-100>}}]\n\n\
         Transcript:\n{t}\n",
        n = settings.count,
        lo = settings.min_seconds as u32,
        hi = settings.max_seconds as u32,
        t = format_transcript(words)
    )
}

#[derive(Deserialize)]
struct RawPick {
    start: f64,
    end: f64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    hook: String,
    #[serde(default)]
    score: f64,
}

/// Turn the model's reply into validated candidates. Tolerates prose and code
/// fences around the JSON array, and drops anything unusable rather than failing.
pub fn parse_llm_response(reply: &str, words: &[Word], duration: f64, settings: &ShortsSettings) -> Result<Vec<ShortCandidate>> {
    let (open, close) = (reply.find('['), reply.rfind(']'));
    let json = match (open, close) {
        (Some(a), Some(b)) if b > a => &reply[a..=b],
        _ => return Err(Error::Invalid("the model's reply had no JSON list".into())),
    };
    let raw: Vec<RawPick> = serde_json::from_str(json).map_err(|e| Error::Invalid(format!("bad model JSON: {e}")))?;
    let sents = sentences(words);
    let mut cands = Vec::new();
    for p in raw {
        if !p.start.is_finite() || !p.end.is_finite() || p.end <= p.start {
            continue;
        }
        let (mut start, mut end) = (p.start, p.end);
        if !sents.is_empty() {
            start = sents.iter().map(|s| s.start).min_by(|a, b| (a - start).abs().total_cmp(&(b - start).abs())).unwrap();
            end = sents.iter().map(|s| s.end).min_by(|a, b| (a - end).abs().total_cmp(&(b - end).abs())).unwrap();
        }
        start = start.max(0.0);
        end = end.min(duration);
        if end - start > settings.max_seconds {
            // Pull the end back to the last sentence boundary that fits.
            let limit = start + settings.max_seconds;
            match sents.iter().map(|s| s.end).rfind(|e| *e <= limit && *e > start) {
                Some(e) => end = e,
                None => end = limit,
            }
        }
        if end - start < settings.min_seconds {
            continue;
        }
        cands.push(ShortCandidate {
            start,
            end,
            title: if p.title.trim().is_empty() { title_from(&p.hook) } else { p.title.trim().to_string() },
            hook: p.hook.trim().to_string(),
            score: p.score.clamp(0.0, 100.0).round() as u32,
            source: "llm".into(),
        });
    }
    Ok(pick_top(cands, settings.count))
}

/// Ask Claude for the best moments. Falls back to the heuristic if the call
/// or the reply is unusable, so the user always gets a list.
#[cfg(feature = "download")]
pub fn find_with_llm(
    words: &[Word],
    duration: f64,
    settings: &ShortsSettings,
    api_key: &str,
    model: &str,
) -> Result<Vec<ShortCandidate>> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 2000,
        "messages": [{ "role": "user", "content": build_prompt(words, settings) }],
    })
    .to_string();
    let mut resp = ureq::post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .send(body)
        .map_err(|e| Error::Download(format!("Claude API request failed: {e}")))?;
    let text = resp.body_mut().read_to_string().map_err(|e| Error::Download(e.to_string()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    let reply: String = v["content"]
        .as_array()
        .map(|a| a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    parse_llm_response(&reply, words, duration, settings)
}

// ----------------------------------------------------------- vertical framing

/// Output size for vertical shorts.
pub const OUT_W: u32 = 1080;
pub const OUT_H: u32 = 1920;

/// How to turn a landscape (or any) frame into 9:16.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Vertical {
    /// Horizontal crop position: 0 = left edge, 0.5 = centre, 1 = right edge.
    pub focus_x: f64,
}

impl Default for Vertical {
    fn default() -> Self {
        Self { focus_x: 0.5 }
    }
}

/// ffmpeg filters that crop a `src_w`x`src_h` frame to 9:16 around `focus_x`
/// and scale to 1080x1920. Sources already narrower than 9:16 are cropped
/// vertically instead of letterboxed.
pub fn vertical_filter(src_w: u32, src_h: u32, v: &Vertical) -> String {
    let ratio = OUT_W as f64 / OUT_H as f64;
    let (cw, ch) = if src_w as f64 / src_h as f64 > ratio {
        ((src_h as f64 * ratio / 2.0).round() as u32 * 2, src_h)
    } else {
        (src_w, (src_w as f64 / ratio / 2.0).round() as u32 * 2)
    };
    let cw = cw.min(src_w);
    let ch = ch.min(src_h);
    let x = ((src_w - cw) as f64 * v.focus_x.clamp(0.0, 1.0)).round() as u32;
    let y = (src_h - ch) / 2;
    format!("crop={cw}:{ch}:{x}:{y},scale={OUT_W}:{OUT_H}:flags=lanczos,setsar=1")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` sentences of 5 words, 2 seconds each, back to back.
    fn talk(n: usize) -> Vec<Word> {
        let mut words = Vec::new();
        for s in 0..n {
            for k in 0..5 {
                let start = s as f64 * 2.0 + k as f64 * 0.4;
                let mut text = format!("word{k}");
                if k == 4 {
                    text.push(if s % 3 == 0 { '?' } else { '.' });
                }
                words.push(Word { text, start, end: start + 0.38, confidence: 1.0 });
            }
        }
        words
    }

    #[test]
    fn sentences_split_on_punctuation_and_gaps() {
        let s = sentences(&talk(4));
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].first, s[0].last), (0, 4));

        let mut w = talk(1);
        w.push(Word { text: "late".into(), start: 30.0, end: 30.4, confidence: 1.0 });
        assert_eq!(sentences(&w).len(), 2, "a long pause starts a new sentence");
    }

    #[test]
    fn heuristic_respects_length_overlap_and_count() {
        let words = talk(60); // 120 seconds
        let s = ShortsSettings { count: 3, min_seconds: 20.0, max_seconds: 40.0 };
        let c = find_heuristic(&words, &s);
        assert!(!c.is_empty() && c.len() <= 3);
        for x in &c {
            assert!(x.duration() >= 20.0 && x.duration() <= 40.0, "{x:?}");
            assert_eq!(x.source, "heuristic");
        }
        for (i, a) in c.iter().enumerate() {
            for b in &c[i + 1..] {
                assert!(!overlaps(a, b));
            }
        }
        assert!(c.windows(2).all(|w| w[0].start < w[1].start), "returned in time order");
    }

    #[test]
    fn heuristic_prefers_a_question_hook() {
        let mut words = talk(40);
        for w in words.iter_mut() {
            w.text = w.text.replace('?', "."); // flat baseline: no hooks anywhere
        }
        // The sentence starting at 40s is a question with a hook word.
        for w in words.iter_mut().filter(|w| (40.0..42.0).contains(&w.start)) {
            if w.text.starts_with("word0") {
                w.text = "Why".into();
            }
            if w.text.starts_with("word4") {
                w.text = "wrong?".into();
            }
        }
        let s = ShortsSettings { count: 1, min_seconds: 20.0, max_seconds: 30.0 };
        let c = find_heuristic(&words, &s);
        assert_eq!(c.len(), 1);
        assert!(c[0].start >= 38.0 && c[0].start <= 42.0, "{c:?}");
    }

    #[test]
    fn empty_or_too_short_transcript_gives_nothing() {
        assert!(find_heuristic(&[], &ShortsSettings::default()).is_empty());
        assert!(find_heuristic(&talk(3), &ShortsSettings::default()).is_empty());
    }

    #[test]
    fn llm_reply_is_parsed_snapped_and_filtered() {
        let words = talk(60);
        let reply = "Sure! Here you go:\n```json\n[\n\
            {\"start\": 0.3, \"end\": 41.7, \"title\": \"Big one\", \"hook\": \"Word0\", \"score\": 92},\n\
            {\"start\": 10.0, \"end\": 50.0, \"title\": \"Overlaps\", \"score\": 80},\n\
            {\"start\": 70.0, \"end\": 75.0, \"title\": \"Too short\", \"score\": 99},\n\
            {\"start\": 80.0, \"end\": 300.0, \"title\": \"Too long\", \"score\": 70},\n\
            {\"start\": 90.0, \"end\": 80.0, \"title\": \"Backwards\", \"score\": 70}\n]\n```";
        let c = parse_llm_response(reply, &words, 120.0, &ShortsSettings::default()).unwrap();
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0].title, "Big one");
        assert_eq!(c[0].start, 0.0, "snapped to the first sentence start");
        assert!((c[0].end - 41.98).abs() < 0.01, "snapped to a sentence end: {}", c[0].end);
        assert_eq!(c[0].source, "llm");
        assert!(c[1].duration() <= 75.0 && c[1].end <= 120.0, "capped: {:?}", c[1]);
    }

    #[test]
    fn llm_garbage_is_an_error_not_a_panic() {
        let words = talk(10);
        assert!(parse_llm_response("I cannot help with that.", &words, 20.0, &ShortsSettings::default()).is_err());
        assert!(parse_llm_response("[{\"start\": \"x\"}]", &words, 20.0, &ShortsSettings::default()).is_err());
    }

    #[test]
    fn prompt_contains_limits_and_timestamps() {
        let p = build_prompt(&talk(4), &ShortsSettings::default());
        assert!(p.contains("between 20 and 75 seconds"));
        assert!(p.contains("[0.0-2.0]"), "{p}");
    }

    #[test]
    fn vertical_filter_crops_landscape_around_focus() {
        let f = vertical_filter(1920, 1080, &Vertical::default());
        assert_eq!(f, "crop=608:1080:656:0,scale=1080:1920:flags=lanczos,setsar=1");
        assert!(vertical_filter(1920, 1080, &Vertical { focus_x: 0.0 }).starts_with("crop=608:1080:0:0"));
        assert!(vertical_filter(1920, 1080, &Vertical { focus_x: 9.0 }).starts_with("crop=608:1080:1312:0"));
    }

    #[test]
    fn vertical_filter_handles_portrait_and_square() {
        assert!(vertical_filter(1080, 1920, &Vertical::default()).starts_with("crop=1080:1920:0:0"));
        let sq = vertical_filter(1000, 1000, &Vertical::default());
        assert!(sq.starts_with("crop=562:1000:219:0"), "{sq}");
        let narrow = vertical_filter(1080, 2400, &Vertical::default());
        assert!(narrow.starts_with("crop=1080:1920:0:240"), "{narrow}");
    }
}
