//! Dev helper: dump everything the UI needs for one file as JSON, so the
//! interface can be exercised in a plain browser with mocked Tauri IPC.
//! Usage: cargo run --example ui_fixture -- <media> <words.json>

use std::path::Path;

use dropcut_core::captions::{self, CaptionSettings};
use dropcut_core::media::{self, FfmpegPaths};
use dropcut_core::profanity::{self, ProfanitySettings};
use dropcut_core::silence::{self, SilenceSettings};
use dropcut_core::transcript::Transcript;
use dropcut_core::{RegionKind, Timeline};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = FfmpegPaths::default();
    let input = Path::new(&args[0]);
    let info = media::probe(&paths, input).unwrap();
    let samples = media::decode_audio(&paths, input).unwrap();
    let levels = silence::frame_levels_db(&samples, media::ANALYSIS_RATE);
    let analysis = silence::detect_from_levels(&levels, &SilenceSettings::default());
    let mut timeline = Timeline::new(info.duration);
    timeline.replace_kind(RegionKind::Silence, analysis.silences.iter().map(|s| (*s, None)));
    let opened_timeline = timeline.clone();
    let transcript: Transcript = serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    timeline.replace_kind(RegionKind::Profanity, profanity::find(&transcript.words, &ProfanitySettings::default()));
    let cues = captions::build_cues(&transcript.words, &timeline, &CaptionSettings::default());
    let out = serde_json::json!({
        "opened": {
            "info": info,
            "peaks": silence::waveform_peaks(&samples, 4000),
            "threshold_db": analysis.threshold_db,
            "timeline": opened_timeline,
        },
        "transcribed": { "transcript": transcript, "timeline": timeline },
        "cues": cues,
    });
    println!("{out}");
}
