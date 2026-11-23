//! Talking to ffmpeg / ffprobe.
//!
//! DropCut ships ffmpeg next to its executable (a Tauri "sidecar"), so the
//! binary locations are configurable rather than assumed to be on `PATH`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Sample rate every analysis step works at. Whisper requires 16 kHz mono.
pub const ANALYSIS_RATE: u32 = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfmpegPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Default for FfmpegPaths {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from(exe_name("ffmpeg")),
            ffprobe: PathBuf::from(exe_name("ffprobe")),
        }
    }
}

impl FfmpegPaths {
    /// Prefer binaries sitting in `dir` (where installers put sidecars),
    /// falling back to whatever is on `PATH`.
    pub fn from_dir_or_path(dir: Option<&Path>) -> Self {
        let mut paths = Self::default();
        if let Some(dir) = dir {
            let ffmpeg = dir.join(exe_name("ffmpeg"));
            let ffprobe = dir.join(exe_name("ffprobe"));
            if ffmpeg.is_file() {
                paths.ffmpeg = ffmpeg;
            }
            if ffprobe.is_file() {
                paths.ffprobe = ffprobe;
            }
        }
        paths
    }

    /// True if ffmpeg and ffprobe both run.
    pub fn command_ok(&self) -> bool {
        [&self.ffmpeg, &self.ffprobe].iter().all(|tool| {
            self.command(tool)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }

    pub(crate) fn command(&self, tool: &Path) -> Command {
        let mut cmd = Command::new(tool);
        hide_console_window(&mut cmd);
        cmd
    }
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Stop Windows from flashing a console window for every ffmpeg call.
#[cfg(windows)]
pub(crate) fn hide_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_console_window(_cmd: &mut Command) {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaInfo {
    pub path: PathBuf,
    /// Seconds.
    pub duration: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
}

/// Read duration and stream layout with ffprobe.
pub fn probe(paths: &FfmpegPaths, input: &Path) -> Result<MediaInfo> {
    if !input.is_file() {
        return Err(Error::NotFound(input.to_path_buf()));
    }
    let output = paths
        .command(&paths.ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(input)
        .output()
        .map_err(|source| Error::ToolMissing { tool: "ffprobe".into(), source })?;
    if !output.status.success() {
        return Err(Error::ToolFailed {
            tool: "ffprobe".into(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    parse_probe(input, &output.stdout)
}

fn parse_probe(input: &Path, json: &[u8]) -> Result<MediaInfo> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        streams: Vec<Stream>,
        format: Option<Format>,
    }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
        avg_frame_rate: Option<String>,
        duration: Option<String>,
        disposition: Option<Disposition>,
    }
    #[derive(Deserialize)]
    struct Disposition {
        #[serde(default)]
        attached_pic: i32,
    }
    #[derive(Deserialize)]
    struct Format {
        duration: Option<String>,
    }

    let probe: Probe = serde_json::from_slice(json)?;
    // Cover art in audio files shows up as a "video" stream; ignore it.
    let video = probe.streams.iter().find(|s| {
        s.codec_type.as_deref() == Some("video")
            && s.disposition.as_ref().map_or(true, |d| d.attached_pic == 0)
    });
    let audio = probe.streams.iter().find(|s| s.codec_type.as_deref() == Some("audio"));

    let duration = probe
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .or_else(|| video.and_then(|v| v.duration.as_deref()))
        .or_else(|| audio.and_then(|a| a.duration.as_deref()))
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0)
        .ok_or_else(|| Error::Invalid("could not read the media duration".into()))?;

    Ok(MediaInfo {
        path: input.to_path_buf(),
        duration,
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        width: video.and_then(|v| v.width),
        height: video.and_then(|v| v.height),
        fps: video.and_then(|v| v.avg_frame_rate.as_deref()).and_then(parse_rate),
    })
}

fn parse_rate(rate: &str) -> Option<f64> {
    let (num, den) = rate.split_once('/')?;
    let (num, den) = (num.parse::<f64>().ok()?, den.parse::<f64>().ok()?);
    (den > 0.0 && num > 0.0).then(|| num / den)
}

/// Decode the first audio track to mono 16 kHz `f32` samples.
pub fn decode_audio(paths: &FfmpegPaths, input: &Path) -> Result<Vec<f32>> {
    let mut child = paths
        .command(&paths.ffmpeg)
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(input)
        .args(["-vn", "-map", "0:a:0", "-ac", "1", "-ar"])
        .arg(ANALYSIS_RATE.to_string())
        .args(["-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::ToolMissing { tool: "ffmpeg".into(), source })?;

    // Drain stderr on a thread so a chatty ffmpeg can't deadlock on a full pipe.
    let mut stderr = child.stderr.take().expect("stderr piped");
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let mut bytes = Vec::new();
    child.stdout.take().expect("stdout piped").read_to_end(&mut bytes)?;
    let status = child.wait()?;
    let err_text = err_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::ToolFailed { tool: "ffmpeg".into(), message: err_text.trim().to_string() });
    }

    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_output() {
        let json = br#"{
            "streams": [
                {"codec_type": "video", "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001"},
                {"codec_type": "audio"}
            ],
            "format": {"duration": "12.500000"}
        }"#;
        let info = parse_probe(Path::new("a.mp4"), json).unwrap();
        assert_eq!(info.duration, 12.5);
        assert!(info.has_video && info.has_audio);
        assert_eq!(info.width, Some(1920));
        assert!((info.fps.unwrap() - 29.97).abs() < 0.01);
    }

    #[test]
    fn cover_art_is_not_video() {
        let json = br#"{
            "streams": [
                {"codec_type": "audio"},
                {"codec_type": "video", "width": 500, "height": 500, "disposition": {"attached_pic": 1}}
            ],
            "format": {"duration": "3.0"}
        }"#;
        let info = parse_probe(Path::new("a.mp3"), json).unwrap();
        assert!(!info.has_video);
    }
}
