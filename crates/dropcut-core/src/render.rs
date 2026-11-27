//! Rendering a [`Timeline`] to a finished file with ffmpeg.
//!
//! Cuts use `select`/`aselect` with a list of kept ranges rather than one
//! `trim` per segment: it streams through the file once, so memory stays flat
//! no matter how many cuts a long video has. The filter graph is written to a
//! script file because hundreds of ranges would overflow the Windows
//! command-line limit.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::media::{FfmpegPaths, MediaInfo};
use crate::profanity::CensorMode;
use crate::shorts::{self, Vertical};
use crate::timeline::{Span, Timeline};
use crate::zoom::{self, ZoomSegment};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VideoEncoder {
    /// Hardware encoder if this machine has one, otherwise x264.
    #[default]
    Auto,
    /// CPU, best compatibility.
    X264,
    /// NVIDIA GPU.
    Nvenc,
    /// Apple Silicon / Intel Mac hardware encoder.
    VideoToolbox,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Draft,
    #[default]
    High,
    Max,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct RenderSettings {
    pub encoder: VideoEncoder,
    pub quality: Quality,
    pub censor_mode: CensorMode,
}

/// Picture effects applied after cutting, on the output timeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VideoEffects {
    /// Punch-in zooms (see [`crate::zoom::plan`]).
    pub zoom: Vec<ZoomSegment>,
    /// Vertical zoom focus, 0 = top, 1 = bottom.
    pub zoom_focus_y: f64,
    /// Crop to 9:16 and scale to 1080x1920 (after zoom, before captions, so
    /// caption scripts should be written for a 1080x1920 frame).
    pub vertical: Option<Vertical>,
    /// A complete ASS script to burn in (see [`crate::burn::to_ass`]).
    pub subtitles: Option<String>,
    /// Folder of font files the subtitles use.
    pub fonts_dir: Option<PathBuf>,
}

/// File names inside the per-render work folder (ffmpeg runs there, so the
/// filter graph never has to escape Windows drive letters).
const SUBS_FILE: &str = "captions.ass";
const FONTS_DIR: &str = "fonts";

/// ffmpeg's `sine` source peaks at 1/8 full scale; this lifts the bleep to
/// about -14 dBFS, roughly as loud as normal speech.
const BLEEP_GAIN: f64 = 1.6;

/// Ranges as an ffmpeg expression: `between(t,0.000,1.500)+between(t,...)`.
fn between_expr(spans: &[Span]) -> String {
    spans.iter().map(|s| format!("between(t\\,{:.3}\\,{:.3})", s.start, s.end)).collect::<Vec<_>>().join("+")
}

/// Build the `-filter_complex` graph. Outputs are labelled `[v]` and `[a]`.
pub fn build_filter_graph(timeline: &Timeline, info: &MediaInfo, censor_mode: CensorMode, effects: &VideoEffects) -> String {
    let kept = timeline.kept();
    let keep = between_expr(&kept);
    let censors = timeline.censors();
    let mut graph = Vec::new();

    if info.has_video {
        let mut chain = format!("[0:v:0]select='{keep}',setpts=N/FRAME_RATE/TB");
        if let (Some(w), Some(h)) = (info.width, info.height) {
            let fps = info.fps.filter(|f| *f > 1.0 && *f < 241.0).unwrap_or(30.0);
            if let Some(z) = zoom::zoompan_filter(&effects.zoom, w, h, fps, effects.zoom_focus_y) {
                chain.push(',');
                chain.push_str(&z);
            }
        }
        if let (Some(v), Some(w), Some(h)) = (&effects.vertical, info.width, info.height) {
            chain.push(',');
            chain.push_str(&shorts::vertical_filter(w, h, v));
        }
        if effects.subtitles.is_some() {
            chain.push_str(&format!(",subtitles={SUBS_FILE}:fontsdir={FONTS_DIR}"));
        }
        graph.push(format!("{chain}[v]"));
    }

    if info.has_audio {
        let mut audio_in = "[0:a:0]aresample=48000".to_string();
        if censors.is_empty() {
            graph.push(format!("{audio_in},aselect='{keep}',asetpts=N/SR/TB[a]"));
        } else {
            let censor = between_expr(&censors);
            audio_in.push_str(&format!(",volume=volume=0:enable='{censor}'"));
            match censor_mode {
                CensorMode::Mute => graph.push(format!("{audio_in},aselect='{keep}',asetpts=N/SR/TB[a]")),
                CensorMode::Bleep => {
                    graph.push(format!("{audio_in}[muted]"));
                    graph.push(format!(
                        "sine=frequency=1000:sample_rate=48000,atrim=duration={:.3},\
                         volume=volume='if({censor}\\,{BLEEP_GAIN}\\,0)':eval=frame[tone]",
                        timeline.duration
                    ));
                    graph.push("[muted][tone]amix=inputs=2:duration=first:normalize=0[mixed]".into());
                    graph.push(format!("[mixed]aselect='{keep}',asetpts=N/SR/TB[a]"));
                }
            }
        }
    }
    graph.join(";\n")
}

/// Video encoders this ffmpeg build lists.
pub fn listed_encoders(paths: &FfmpegPaths) -> Vec<String> {
    let Ok(out) = paths.command(&paths.ffmpeg).args(["-hide_banner", "-encoders"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let flags = parts.next()?;
            flags.starts_with('V').then(|| parts.next().map(str::to_string)).flatten()
        })
        .collect()
}

/// An encoder is only usable if it actually opens: nvenc is listed in most
/// builds even on machines with no NVIDIA card.
pub fn encoder_works(paths: &FfmpegPaths, encoder: &str) -> bool {
    paths
        .command(&paths.ffmpeg)
        .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=256x256:d=0.1", "-c:v", encoder, "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn resolve_encoder(paths: &FfmpegPaths, choice: VideoEncoder) -> VideoEncoder {
    if choice != VideoEncoder::Auto {
        return choice;
    }
    let listed = listed_encoders(paths);
    let candidates: &[(VideoEncoder, &str)] = if cfg!(target_os = "macos") {
        &[(VideoEncoder::VideoToolbox, "h264_videotoolbox")]
    } else {
        &[(VideoEncoder::Nvenc, "h264_nvenc")]
    };
    for (enc, name) in candidates {
        if listed.iter().any(|l| l == name) && encoder_works(paths, name) {
            return *enc;
        }
    }
    VideoEncoder::X264
}

fn encoder_args(encoder: VideoEncoder, quality: Quality) -> Vec<String> {
    let q = |draft: u32, high: u32, max: u32| match quality {
        Quality::Draft => draft,
        Quality::High => high,
        Quality::Max => max,
    };
    let args: Vec<String> = match encoder {
        VideoEncoder::Auto | VideoEncoder::X264 => vec![
            "-c:v".into(), "libx264".into(),
            "-preset".into(), if quality == Quality::Draft { "veryfast" } else { "medium" }.into(),
            "-crf".into(), q(26, 19, 16).to_string(),
        ],
        VideoEncoder::Nvenc => vec![
            "-c:v".into(), "h264_nvenc".into(),
            "-preset".into(), "p5".into(),
            "-rc".into(), "vbr".into(),
            "-cq".into(), q(28, 21, 18).to_string(),
            "-b:v".into(), "0".into(),
        ],
        VideoEncoder::VideoToolbox => vec![
            "-c:v".into(), "h264_videotoolbox".into(),
            "-q:v".into(), q(50, 65, 75).to_string(),
        ],
    };
    let mut args = args;
    args.extend(["-pix_fmt".into(), "yuv420p".into()]);
    args
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RenderOutcome {
    pub output: PathBuf,
    pub encoder: VideoEncoder,
    pub duration: f64,
}

/// Render `timeline` from `info.path` to `output`. `progress` gets 0.0..=1.0.
pub fn render(
    paths: &FfmpegPaths,
    info: &MediaInfo,
    timeline: &Timeline,
    output: &Path,
    settings: &RenderSettings,
    effects: &VideoEffects,
    mut progress: impl FnMut(f64),
    cancel: Option<Arc<AtomicBool>>,
) -> Result<RenderOutcome> {
    if timeline.kept().is_empty() {
        return Err(Error::Invalid("everything is cut; nothing left to export".into()));
    }
    let encoder = if info.has_video { resolve_encoder(paths, settings.encoder) } else { VideoEncoder::X264 };
    match run_ffmpeg(paths, info, timeline, output, settings, effects, encoder, &mut progress, cancel.clone()) {
        Ok(o) => Ok(o),
        // A hardware encoder can still fail mid-way (driver, unsupported size);
        // fall back to the CPU rather than give up.
        Err(Error::ToolFailed { .. }) if encoder != VideoEncoder::X264 => {
            run_ffmpeg(paths, info, timeline, output, settings, effects, VideoEncoder::X264, &mut progress, cancel)
        }
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_ffmpeg(
    paths: &FfmpegPaths,
    info: &MediaInfo,
    timeline: &Timeline,
    output: &Path,
    settings: &RenderSettings,
    effects: &VideoEffects,
    encoder: VideoEncoder,
    progress: &mut dyn FnMut(f64),
    cancel: Option<Arc<AtomicBool>>,
) -> Result<RenderOutcome> {
    let work = WorkDir::new()?;
    let graph = build_filter_graph(timeline, info, settings.censor_mode, effects);
    std::fs::write(work.0.join("filter.txt"), &graph)?;
    if let Some(ass) = &effects.subtitles {
        std::fs::write(work.0.join(SUBS_FILE), ass)?;
        let fonts = work.0.join(FONTS_DIR);
        std::fs::create_dir_all(&fonts)?;
        if let Some(dir) = &effects.fonts_dir {
            for entry in std::fs::read_dir(dir)?.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf")) {
                    std::fs::copy(&p, fonts.join(entry.file_name()))?;
                }
            }
        }
    }
    // ffmpeg runs inside the work folder, so the media paths must be absolute.
    let input = std::path::absolute(&info.path)?;
    let output = std::path::absolute(output)?;
    let output = output.as_path();

    let mut cmd = paths.command(&paths.ffmpeg);
    cmd.current_dir(&work.0)
        .args(["-nostdin", "-y", "-hide_banner", "-v", "error", "-progress", "pipe:1", "-i"])
        .arg(&input)
        .args(["-filter_complex_script", "filter.txt"]);
    if info.has_video {
        cmd.args(["-map", "[v]"]).args(encoder_args(encoder, settings.quality));
    }
    if info.has_audio {
        cmd.args(["-map", "[a]", "-c:a", "aac", "-b:a", "192k"]);
    }
    if output.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("mov")) {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.arg(output).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|source| Error::ToolMissing { tool: "ffmpeg".into(), source })?;
    let mut stderr = child.stderr.take().expect("stderr piped");
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let total = timeline.output_duration().max(0.001);
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut cancelled = false;
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            let _ = child.kill();
            cancelled = true;
            break;
        }
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            progress((us / 1_000_000.0 / total).clamp(0.0, 1.0));
        }
    }

    let status = child.wait()?;
    let err_text = err_reader.join().unwrap_or_default();
    if cancelled {
        let _ = std::fs::remove_file(output);
        return Err(Error::Cancelled);
    }
    if !status.success() {
        return Err(Error::ToolFailed { tool: "ffmpeg".into(), message: err_text.trim().to_string() });
    }
    progress(1.0);
    Ok(RenderOutcome { output: output.to_path_buf(), encoder, duration: total })
}

/// A temporary folder removed when dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Result<Self> {
        use std::sync::atomic::AtomicU32;
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dropcut-render-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::RegionKind;

    fn info(has_video: bool) -> MediaInfo {
        MediaInfo { path: "in.mp4".into(), duration: 10.0, has_video, has_audio: true, width: Some(1920), height: Some(1080), fps: Some(30.0) }
    }

    fn timeline() -> Timeline {
        let mut t = Timeline::new(10.0);
        t.add(RegionKind::Silence, Span::new(2.0, 3.0), None);
        t
    }

    #[test]
    fn graph_selects_kept_ranges() {
        let g = build_filter_graph(&timeline(), &info(true), CensorMode::Mute, &VideoEffects::default());
        assert!(g.contains("[0:v:0]select='between(t\\,0.000\\,2.000)+between(t\\,3.000\\,10.000)'"), "{g}");
        assert!(g.contains("[a]") && g.contains("[v]"));
        assert!(!g.contains("volume"));
    }

    #[test]
    fn effects_follow_the_cut() {
        let fx = VideoEffects {
            zoom: vec![ZoomSegment { start: 1.0, end: 2.0, scale: 1.2 }],
            zoom_focus_y: 0.4,
            vertical: None,
            subtitles: Some(String::new()),
            fonts_dir: None,
        };
        let g = build_filter_graph(&timeline(), &info(true), CensorMode::Mute, &fx);
        let v = g.lines().find(|l| l.starts_with("[0:v:0]")).unwrap();
        let (sel, zp, sub) = (v.find("select=").unwrap(), v.find("zoompan=").unwrap(), v.find("subtitles=").unwrap());
        assert!(sel < zp && zp < sub, "{v}");
        assert!(v.trim_end_matches(';').ends_with("subtitles=captions.ass:fontsdir=fonts[v]"), "{v}");
    }

    #[test]
    fn vertical_crop_sits_between_zoom_and_captions() {
        let fx = VideoEffects {
            zoom: vec![ZoomSegment { start: 1.0, end: 2.0, scale: 1.2 }],
            zoom_focus_y: 0.4,
            vertical: Some(Vertical::default()),
            subtitles: Some(String::new()),
            fonts_dir: None,
        };
        let g = build_filter_graph(&timeline(), &info(true), CensorMode::Mute, &fx);
        let v = g.lines().find(|l| l.starts_with("[0:v:0]")).unwrap();
        let (zp, cr, sub) = (v.find("zoompan=").unwrap(), v.find("crop=").unwrap(), v.find("subtitles=").unwrap());
        assert!(zp < cr && cr < sub, "{v}");
        assert!(v.contains("scale=1080:1920"));
    }

    #[test]
    fn audio_only_has_no_video_chain() {
        let g = build_filter_graph(&timeline(), &info(false), CensorMode::Mute, &VideoEffects::default());
        assert!(!g.contains("[v]"));
    }

    #[test]
    fn bleep_mixes_a_tone() {
        let mut t = timeline();
        t.add(RegionKind::Profanity, Span::new(5.0, 5.4), None);
        let g = build_filter_graph(&t, &info(true), CensorMode::Bleep, &VideoEffects::default());
        assert!(g.contains("sine=frequency=1000"));
        assert!(g.contains("amix"));
    }
}
