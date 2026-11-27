//! Hand-off to a scheduler (Metricool): captions, hashtags and a manifest.
//!
//! Every exported short is recorded in `dropcut-publish.json` beside the
//! clips. The manifest holds what a scheduler needs that is not in the video
//! itself: a ready-to-post caption, hashtags and the clip's length. Re-exporting
//! into the same folder updates the existing entry instead of duplicating it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::shorts::ShortCandidate;
use crate::transcript::{normalize, Word};
use crate::Result;

pub const MANIFEST_FILE: &str = "dropcut-publish.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestClip {
    /// File name inside the manifest's folder.
    pub file: String,
    pub title: String,
    pub hook: String,
    /// Ready-to-post text: the hook plus hashtags.
    pub caption: String,
    pub hashtags: Vec<String>,
    /// Length of the exported video in seconds.
    pub duration: f64,
    pub score: u32,
    /// Where the clip came from in the source video.
    pub source_start: f64,
    pub source_end: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: u32,
    /// Name of the source video the clips were cut from.
    pub source: String,
    pub clips: Vec<ManifestClip>,
}

const STOP: &[&str] = &[
    "about", "after", "again", "also", "because", "been", "before", "being", "could", "does", "doing", "dont",
    "from", "have", "here", "into", "just", "like", "make", "more", "most", "much", "only", "other", "really",
    "right", "since", "some", "than", "that", "their", "them", "then", "there", "these", "they", "thing", "things",
    "this", "those", "very", "want", "were", "what", "when", "where", "which", "while", "will", "with", "would",
    "your", "youre", "gonna", "know", "think", "going", "yeah", "okay", "well", "kind", "sort", "lot", "got",
];

/// Up to `n` hashtags from the most repeated meaningful words in `words`.
/// Ties go to the word spoken first, so the result is stable.
pub fn hashtags(words: &[Word], n: usize) -> Vec<String> {
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
    for (i, w) in words.iter().enumerate() {
        let norm = normalize(&w.text);
        if norm.chars().count() < 4 || STOP.contains(&norm.as_str()) || norm.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        counts.entry(norm).or_insert((0, i)).0 += 1;
    }
    let mut v: Vec<(String, (usize, usize))> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.1 .1.cmp(&b.1 .1)));
    v.into_iter().take(n).map(|(w, _)| format!("#{w}")).collect()
}

/// Entry for a clip cut from `pick`, using the words spoken inside it.
pub fn entry_for(file: &str, pick: &ShortCandidate, words: &[Word], duration: f64) -> ManifestClip {
    let inside: Vec<Word> = words.iter().filter(|w| w.end > pick.start && w.start < pick.end).cloned().collect();
    let tags = hashtags(&inside, 4);
    let lead = if pick.hook.trim().is_empty() { pick.title.trim() } else { pick.hook.trim() };
    let caption = if tags.is_empty() { lead.to_string() } else { format!("{lead}\n\n{}", tags.join(" ")) };
    ManifestClip {
        file: file.to_string(),
        title: pick.title.clone(),
        hook: pick.hook.clone(),
        caption,
        hashtags: tags,
        duration,
        score: pick.score,
        source_start: pick.start,
        source_end: pick.end,
    }
}

/// Add (or replace, by file name) `clip` in the manifest inside `dir`.
/// Returns the manifest path. A corrupt existing manifest is replaced.
pub fn record(dir: &Path, source: &str, clip: ManifestClip) -> Result<PathBuf> {
    let path = dir.join(MANIFEST_FILE);
    let mut m: Manifest = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    m.version = 1;
    m.source = source.to_string();
    match m.clips.iter_mut().find(|c| c.file == clip.file) {
        Some(existing) => *existing = clip,
        None => m.clips.push(clip),
    }
    // Write beside, then rename, so a crash never leaves half a manifest.
    let tmp = dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(&m)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64) -> Word {
        Word { text: text.into(), start, end: start + 0.4, confidence: 1.0 }
    }

    fn pick() -> ShortCandidate {
        ShortCandidate {
            start: 10.0,
            end: 40.0,
            title: "Why people quit".into(),
            hook: "Why does everyone quit?".into(),
            score: 90,
            source: "llm".into(),
        }
    }

    #[test]
    fn hashtags_rank_by_frequency_and_skip_filler() {
        let words: Vec<Word> = "Training training is really about consistency, and consistency beats motivation. Training!"
            .split_whitespace()
            .enumerate()
            .map(|(i, t)| w(t, i as f64))
            .collect();
        assert_eq!(hashtags(&words, 3), vec!["#training", "#consistency", "#beats"]);
        assert!(hashtags(&words, 0).is_empty());
        assert!(hashtags(&[], 3).is_empty());
    }

    #[test]
    fn hashtags_ignore_short_words_numbers_and_punctuation() {
        let words = vec![w("2024", 0.0), w("the", 1.0), w("Money!", 2.0), w("money,", 3.0)];
        assert_eq!(hashtags(&words, 5), vec!["#money"]);
    }

    #[test]
    fn entry_uses_only_words_inside_the_pick() {
        let words = vec![w("outside", 1.0), w("kayaking", 12.0), w("kayaking", 20.0), w("later", 90.0)];
        let e = entry_for("a.mp4", &pick(), &words, 29.5);
        assert_eq!(e.hashtags, vec!["#kayaking"]);
        assert_eq!(e.caption, "Why does everyone quit?\n\n#kayaking");
        assert_eq!((e.file.as_str(), e.duration, e.score), ("a.mp4", 29.5, 90));
    }

    #[test]
    fn caption_falls_back_to_title_without_hook_or_tags() {
        let mut p = pick();
        p.hook = "  ".into();
        assert_eq!(entry_for("a.mp4", &p, &[], 20.0).caption, "Why people quit");
    }

    #[test]
    fn record_appends_updates_and_survives_corruption() {
        let dir = std::env::temp_dir().join(format!("dropcut-pub-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = entry_for("a.mp4", &pick(), &[], 20.0);
        let b = entry_for("b.mp4", &pick(), &[], 25.0);
        record(&dir, "src.mp4", a.clone()).unwrap();
        record(&dir, "src.mp4", b).unwrap();
        let mut a2 = a;
        a2.duration = 21.0;
        let path = record(&dir, "src.mp4", a2).unwrap();
        let m: Manifest = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(m.clips.len(), 2, "re-export replaces, not duplicates");
        assert_eq!(m.clips[0].duration, 21.0);
        assert_eq!((m.version, m.source.as_str()), (1, "src.mp4"));

        std::fs::write(&path, "{ not json").unwrap();
        record(&dir, "src.mp4", entry_for("c.mp4", &pick(), &[], 30.0)).unwrap();
        let m: Manifest = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(m.clips.len(), 1);
        assert!(!dir.join(format!("{MANIFEST_FILE}.tmp")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
