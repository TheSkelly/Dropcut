//! Burned-in, animated captions.
//!
//! Captions are written as an ASS subtitle script (the format libass renders
//! inside ffmpeg), positioned and sized relative to the video so they look
//! the same on a 1080p landscape video and a 9:16 short.

use serde::{Deserialize, Serialize};

use crate::captions::Cue;

/// Family name of the bundled caption font (Poppins ExtraBold, SIL OFL).
pub const FONT_FAMILY: &str = "Poppins";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptionStyle {
    /// White text with a dark outline, whole line at once.
    Clean,
    /// Whole line shown; the word being spoken turns yellow.
    #[default]
    Highlight,
    /// Bold uppercase; the spoken word is coloured and enlarged, lines pop in.
    Pop,
    /// Words fill with colour left to right as they're spoken.
    Karaoke,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptionPosition {
    Bottom,
    /// Lower third, clear of platform UI on TikTok / Reels / Shorts.
    #[default]
    Raised,
    Center,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BurnSettings {
    pub enabled: bool,
    pub style: CaptionStyle,
    pub position: CaptionPosition,
    /// Text size multiplier, 1.0 = default.
    pub size: f64,
    /// Highlight colour as "#RRGGBB".
    pub highlight: String,
}

impl Default for BurnSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            style: CaptionStyle::Highlight,
            position: CaptionPosition::Raised,
            size: 1.0,
            highlight: "#FFD23F".into(),
        }
    }
}

/// "#RRGGBB" -> ASS "&H00BBGGRR". Falls back to yellow on bad input.
fn ass_color(hex: &str) -> String {
    let h = hex.trim_start_matches('#');
    if h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("&H00{}{}{}", &h[4..6], &h[2..4], &h[0..2]).to_uppercase()
    } else {
        "&H003FD2FF".into()
    }
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, (cs / 6000) % 60, (cs / 100) % 60, cs % 100)
}

/// Make transcript text safe inside an ASS event.
fn escape(text: &str) -> String {
    text.replace('\\', "\u{29F5}").replace('{', "(").replace('}', ")").replace('\n', " ")
}

/// Build a complete ASS script for `cues` on a `width`x`height` video.
pub fn to_ass(cues: &[Cue], settings: &BurnSettings, width: u32, height: u32) -> String {
    let (w, h) = (width.max(2) as f64, height.max(2) as f64);
    // Size against the shorter side so vertical and horizontal video match.
    let base = w.min(h);
    let pop = settings.style == CaptionStyle::Pop;
    let font_size = (base * if pop { 0.085 } else { 0.068 } * settings.size.clamp(0.4, 2.5)).round();
    let outline = (font_size * 0.09).max(2.0).round();
    let shadow = (font_size * 0.04).round();
    let (alignment, margin_v) = match settings.position {
        CaptionPosition::Bottom => (2, h * 0.07),
        CaptionPosition::Raised => (2, h * 0.22),
        CaptionPosition::Center => (5, 0.0),
    };
    let margin_h = (w * 0.08).round();
    let hl = ass_color(&settings.highlight);
    let white = "&H00FFFFFF";
    // Karaoke: SecondaryColour is the "not yet sung" colour.
    let (primary, secondary) =
        if settings.style == CaptionStyle::Karaoke { (hl.as_str(), "&H00FFFFFF") } else { (white, "&H00FFFFFF") };

    let mut out = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,{FONT_FAMILY},{font_size},{primary},{secondary},&H00000000,&H80000000,-1,0,0,0,100,100,0,0,1,{outline},{shadow},{alignment},{margin_h},{margin_h},{margin_v},1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        w = w as u32,
        h = h as u32,
        margin_v = margin_v.round(),
    );

    let line = |out: &mut String, start: f64, end: f64, text: &str| {
        if end > start {
            out.push_str(&format!("Dialogue: 0,{},{},Default,,0,0,0,,{}\n", ass_time(start), ass_time(end), text));
        }
    };

    for cue in cues {
        let words: Vec<String> = cue
            .words
            .iter()
            .map(|w| escape(&if pop { w.text.to_uppercase() } else { w.text.clone() }))
            .collect();
        if words.is_empty() {
            continue;
        }
        match settings.style {
            CaptionStyle::Clean => line(&mut out, cue.start, cue.end, &words.join(" ")),
            CaptionStyle::Karaoke => {
                // \kf durations (centiseconds) cover the gap before each word too.
                let mut cursor = cue.start;
                let mut text = String::new();
                for (i, w) in cue.words.iter().enumerate() {
                    let end = cue.words.get(i + 1).map(|n| n.start).unwrap_or(w.end).max(cursor);
                    let cs = ((end - cursor) * 100.0).round().max(1.0) as u64;
                    text.push_str(&format!("{{\\kf{cs}}}{} ", words[i]));
                    cursor = end;
                }
                line(&mut out, cue.start, cue.end, text.trim_end());
            }
            CaptionStyle::Highlight | CaptionStyle::Pop => {
                // One event per spoken word, with that word styled.
                for i in 0..cue.words.len() {
                    let start = if i == 0 { cue.start } else { cue.words[i].start };
                    let end = cue.words.get(i + 1).map(|n| n.start).unwrap_or(cue.end);
                    let mut text = String::new();
                    if pop && i == 0 {
                        // Whole line pops in: 80% -> 100% over 90 ms.
                        text.push_str("{\\fscx80\\fscy80\\t(0,90,\\fscx100\\fscy100)}");
                    }
                    for (j, word) in words.iter().enumerate() {
                        if j > 0 {
                            text.push(' ');
                        }
                        if j == i {
                            let grow = if pop { "\\fscx112\\fscy112" } else { "" };
                            text.push_str(&format!("{{\\c{hl}{grow}}}{word}{{\\r}}"));
                            if pop && i == 0 {
                                // \r resets the pop-in transform; re-apply for the rest.
                                text.push_str("{\\fscx80\\fscy80\\t(0,90,\\fscx100\\fscy100)}");
                            }
                        } else {
                            text.push_str(word);
                        }
                    }
                    line(&mut out, start, end, &text);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::CueWord;

    fn cue() -> Cue {
        let words = vec![
            CueWord { text: "hello".into(), start: 1.0, end: 1.4, source: None },
            CueWord { text: "world".into(), start: 1.5, end: 2.0, source: None },
        ];
        Cue { start: 1.0, end: 2.3, text: "hello world".into(), words }
    }

    #[test]
    fn colors_and_times() {
        assert_eq!(ass_color("#FFD23F"), "&H003FD2FF");
        assert_eq!(ass_color("nope"), "&H003FD2FF");
        assert_eq!(ass_time(3661.256), "1:01:01.26");
    }

    #[test]
    fn highlight_makes_one_event_per_word() {
        let s = BurnSettings { enabled: true, ..Default::default() };
        let ass = to_ass(&[cue()], &s, 1920, 1080);
        let events: Vec<_> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("0:00:01.00,0:00:01.50"));
        assert!(events[0].contains("{\\c&H003FD2FF}hello{\\r} world"));
        assert!(events[1].contains("hello {\\c&H003FD2FF}world{\\r}"));
        assert!(ass.contains("PlayResX: 1920") && ass.contains("Style: Default,Poppins,73,"));
    }

    #[test]
    fn pop_is_uppercase_with_pop_in() {
        let s = BurnSettings { enabled: true, style: CaptionStyle::Pop, ..Default::default() };
        let ass = to_ass(&[cue()], &s, 1080, 1920);
        assert!(ass.contains("HELLO") && ass.contains("\\t(0,90,"));
        // Sized from the short side, so vertical video isn't huge.
        assert!(ass.contains("Style: Default,Poppins,92,"));
    }

    #[test]
    fn karaoke_uses_fill_timing() {
        let s = BurnSettings { enabled: true, style: CaptionStyle::Karaoke, ..Default::default() };
        let ass = to_ass(&[cue()], &s, 1920, 1080);
        let ev = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        assert!(ev.ends_with("{\\kf50}hello {\\kf50}world"), "{ev}");
    }

    #[test]
    fn escapes_override_braces() {
        assert_eq!(escape("a{b}c\\d"), "a(b)c\u{29F5}d");
    }
}
