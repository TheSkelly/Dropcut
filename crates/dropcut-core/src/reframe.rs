//! Face-following 9:16 reframing for vertical shorts.
//!
//! The pipeline is deliberately split so everything except the neural net can
//! be unit tested with plain numbers:
//!
//! 1. [`sample_frames`] pulls small RGB frames out of ffmpeg a few times a second.
//! 2. A [`FaceDetector`] finds faces in each frame ([`UltraFace`], a 1 MB
//!    model that runs on the CPU through pure-Rust `tract`; feature `faces`).
//! 3. [`pick_subjects`] follows one person over time (it won't flip between
//!    two people just because one is momentarily larger).
//! 4. [`smooth`] turns the raw positions into a calm camera path: gaps are
//!    bridged, glitches removed, small drift ignored, real moves eased in.
//! 5. [`to_output_keys`] moves that path onto the edited timeline, and
//!    [`crate::shorts::vertical_filter`] turns it into an ffmpeg crop that
//!    pans with the speaker.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;

use crate::media::FfmpegPaths;
use crate::timeline::Timeline;
use crate::{Error, Result};

/// Detector input size. Frames are scaled to exactly this before detection.
pub const DET_W: usize = 320;
pub const DET_H: usize = 240;
const FRAME_BYTES: usize = DET_W * DET_H * 3;

/// A face, everything normalised to 0..1 of the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub score: f32,
}

impl Face {
    pub fn area(&self) -> f64 {
        self.w * self.h
    }
}

pub trait FaceDetector {
    /// `rgb` is `DET_W * DET_H * 3` bytes, row-major RGB.
    fn detect(&mut self, rgb: &[u8]) -> Result<Vec<Face>>;
}

// ------------------------------------------------------------ detection output

fn iou(a: &Face, b: &Face) -> f64 {
    let (ax1, ax2, ay1, ay2) = (a.cx - a.w / 2.0, a.cx + a.w / 2.0, a.cy - a.h / 2.0, a.cy + a.h / 2.0);
    let (bx1, bx2, by1, by2) = (b.cx - b.w / 2.0, b.cx + b.w / 2.0, b.cy - b.h / 2.0, b.cy + b.h / 2.0);
    let iw = (ax2.min(bx2) - ax1.max(bx1)).max(0.0);
    let ih = (ay2.min(by2) - ay1.max(by1)).max(0.0);
    let inter = iw * ih;
    let union = a.area() + b.area() - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Turn the model's raw tensors into faces: `scores` is `[n, 2]` (background,
/// face), `boxes` is `[n, 4]` as normalised `x1, y1, x2, y2`. Boxes below
/// `threshold` are dropped and overlapping ones merged (non-maximum suppression).
pub fn decode_detections(scores: &[f32], boxes: &[f32], threshold: f32) -> Vec<Face> {
    let n = (scores.len() / 2).min(boxes.len() / 4);
    let mut found: Vec<Face> = (0..n)
        .filter_map(|i| {
            let p = scores[i * 2 + 1];
            if p < threshold {
                return None;
            }
            let b = &boxes[i * 4..i * 4 + 4];
            let (x1, y1) = (b[0].clamp(0.0, 1.0) as f64, b[1].clamp(0.0, 1.0) as f64);
            let (x2, y2) = (b[2].clamp(0.0, 1.0) as f64, b[3].clamp(0.0, 1.0) as f64);
            if x2 <= x1 || y2 <= y1 {
                return None;
            }
            Some(Face { cx: (x1 + x2) / 2.0, cy: (y1 + y2) / 2.0, w: x2 - x1, h: y2 - y1, score: p })
        })
        .collect();
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Face> = Vec::new();
    for f in found {
        if kept.iter().all(|k| iou(k, &f) < 0.3) {
            kept.push(f);
        }
    }
    kept
}

#[cfg(feature = "faces")]
mod ultra {
    use super::*;
    use tract_onnx::prelude::*;

    const MODEL: &[u8] = include_bytes!("../models/version-RFB-320.onnx");

    /// Ultra-Light-Fast-Generic-Face-Detector (RFB-320), MIT licensed.
    pub struct UltraFace {
        model: TypedRunnableModel<TypedModel>,
        pub threshold: f32,
    }

    impl UltraFace {
        pub fn new() -> Result<Self> {
            let fail = |e: TractError| Error::Invalid(format!("face model failed to load: {e}"));
            let model = tract_onnx::onnx()
                .model_for_read(&mut std::io::Cursor::new(MODEL))
                .map_err(fail)?
                .with_input_fact(0, f32::fact([1, 3, DET_H, DET_W]).into())
                .map_err(fail)?
                .into_optimized()
                .map_err(fail)?
                .into_runnable()
                .map_err(fail)?;
            Ok(Self { model, threshold: 0.7 })
        }
    }

    impl FaceDetector for UltraFace {
        fn detect(&mut self, rgb: &[u8]) -> Result<Vec<Face>> {
            if rgb.len() != FRAME_BYTES {
                return Err(Error::Invalid("face detector got a frame of the wrong size".into()));
            }
            let fail = |e: TractError| Error::Invalid(format!("face detection failed: {e}"));
            let input: Tensor = tract_ndarray::Array4::from_shape_fn((1, 3, DET_H, DET_W), |(_, c, y, x)| {
                (rgb[(y * DET_W + x) * 3 + c] as f32 - 127.0) / 128.0
            })
            .into();
            let out = self.model.run(tvec!(input.into())).map_err(fail)?;
            let scores = out[0].to_array_view::<f32>().map_err(fail)?;
            let boxes = out[1].to_array_view::<f32>().map_err(fail)?;
            let (s, b) = (scores.as_slice().unwrap_or(&[]), boxes.as_slice().unwrap_or(&[]));
            Ok(decode_detections(s, b, self.threshold))
        }
    }
}

#[cfg(feature = "faces")]
pub use ultra::UltraFace;

// ------------------------------------------------------------------- sampling

/// Decode `[start, end)` of `input` to `fps` small RGB frames and call
/// `each(source_time, rgb)` for every one. Stops early if `each` errors.
pub fn sample_frames(
    paths: &FfmpegPaths,
    input: &Path,
    start: f64,
    end: f64,
    fps: f64,
    mut each: impl FnMut(f64, &[u8]) -> Result<()>,
) -> Result<()> {
    let vf = format!("fps={fps},scale={DET_W}:{DET_H}:flags=bilinear,format=rgb24");
    let mut child = paths
        .command(&paths.ffmpeg)
        .args(["-nostdin", "-v", "error", "-ss"])
        .arg(format!("{start:.3}"))
        .arg("-t")
        .arg(format!("{:.3}", (end - start).max(0.0)))
        .arg("-i")
        .arg(input)
        .args(["-an", "-vf", &vf, "-f", "rawvideo", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| Error::ToolMissing { tool: "ffmpeg".into(), source })?;
    let mut out = child.stdout.take().expect("stdout piped");
    let mut buf = vec![0u8; FRAME_BYTES];
    let mut i = 0usize;
    let result = loop {
        match out.read_exact(&mut buf) {
            Ok(()) => {
                if let Err(e) = each(start + i as f64 / fps, &buf) {
                    break Err(e);
                }
                i += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break Ok(()),
            Err(e) => break Err(e.into()),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

// ------------------------------------------------------------------- tracking

/// A face this far (normalised x) from the one being followed is the same person.
const SAME_PERSON: f64 = 0.2;
/// Samples with no matching face before switching to someone else (~1.3 s at 3 fps).
const SWITCH_AFTER: usize = 4;

/// Follow one person through the clip. Returns `(time, face_centre_x)` per
/// sample, `None` while the subject is not visible. The subject only changes
/// after nobody resembling them has been seen for a while, so two people
/// talking don't make the camera ping-pong.
pub fn pick_subjects(frames: &[(f64, Vec<Face>)]) -> Vec<(f64, Option<f64>)> {
    let mut cur: Option<f64> = None;
    let mut missing = 0usize;
    let largest = |faces: &[Face]| faces.iter().copied().max_by(|a, b| a.area().total_cmp(&b.area()));
    frames
        .iter()
        .map(|(t, faces)| {
            if faces.is_empty() {
                missing += 1;
                return (*t, None);
            }
            let chosen = match cur {
                Some(c) => {
                    let near = faces
                        .iter()
                        .copied()
                        .filter(|f| (f.cx - c).abs() <= SAME_PERSON)
                        .min_by(|a, b| (a.cx - c).abs().total_cmp(&(b.cx - c).abs()));
                    match near {
                        Some(f) => {
                            missing = 0;
                            Some(f)
                        }
                        None => {
                            missing += 1;
                            if missing >= SWITCH_AFTER {
                                missing = 0;
                                largest(faces)
                            } else {
                                None
                            }
                        }
                    }
                }
                None => largest(faces),
            };
            if let Some(f) = chosen {
                cur = Some(f.cx);
            }
            (*t, chosen.map(|f| f.cx))
        })
        .collect()
}

/// Moves smaller than this (fraction of frame width) are ignored.
const DEADBAND: f64 = 0.025;
/// Jumps bigger than this are treated as a cut or a speaker change and followed at once.
const SNAP: f64 = 0.30;
/// How quickly a real move is followed (0..1 per sample).
const EASE: f64 = 0.35;

/// Calm camera path from raw subject positions: bridge gaps, drop one-frame
/// glitches (median of 5), hold still through small drift and ease into real
/// moves. Empty if the subject was never seen.
pub fn smooth(samples: &[(f64, Option<f64>)]) -> Vec<(f64, f64)> {
    let known: Vec<(f64, f64)> = samples.iter().filter_map(|(t, c)| c.map(|c| (*t, c))).collect();
    if known.is_empty() {
        return Vec::new();
    }
    // Fill gaps by linear interpolation, holding the ends.
    let filled: Vec<f64> = samples
        .iter()
        .map(|(t, _)| {
            let after = known.iter().position(|(kt, _)| kt >= t);
            match after {
                None => known.last().unwrap().1,
                Some(0) => known[0].1,
                Some(i) => {
                    let ((t0, c0), (t1, c1)) = (known[i - 1], known[i]);
                    if t1 - t0 <= 1e-9 {
                        c1
                    } else {
                        c0 + (c1 - c0) * (t - t0) / (t1 - t0)
                    }
                }
            }
        })
        .collect();
    // Median of 5 removes single-sample detector glitches.
    let med: Vec<f64> = (0..filled.len())
        .map(|i| {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(filled.len());
            let mut w = filled[lo..hi].to_vec();
            w.sort_by(f64::total_cmp);
            w[w.len() / 2]
        })
        .collect();
    let mut cur = med[0];
    samples
        .iter()
        .zip(&med)
        .map(|((t, _), &v)| {
            let d = v - cur;
            if d.abs() > SNAP {
                cur = v;
            } else if d.abs() > DEADBAND {
                cur += EASE * d;
            }
            (*t, cur)
        })
        .collect()
}

/// Everything [`track_faces`] learned about a clip.
#[derive(Debug, Clone, Default)]
pub struct Track {
    /// Smoothed `(source_time, centre_x)`; empty if no face was ever found.
    pub path: Vec<(f64, f64)>,
    /// Frames sampled, and how many of them had a face.
    pub frames: usize,
    pub with_faces: usize,
}

impl Track {
    /// Enough of the clip had a face for panning to be trustworthy.
    pub fn is_usable(&self) -> bool {
        self.frames > 0 && !self.path.is_empty() && self.with_faces as f64 >= 0.25 * self.frames as f64
    }
}

/// Detect and follow the speaker between `start` and `end` of `input`.
pub fn track_faces(
    paths: &FfmpegPaths,
    input: &Path,
    start: f64,
    end: f64,
    detector: &mut dyn FaceDetector,
    fps: f64,
    mut progress: impl FnMut(f64),
) -> Result<Track> {
    let total = ((end - start) * fps).max(1.0);
    let mut frames: Vec<(f64, Vec<Face>)> = Vec::new();
    sample_frames(paths, input, start, end, fps, |t, rgb| {
        frames.push((t, detector.detect(rgb)?));
        progress((frames.len() as f64 / total).min(1.0));
        Ok(())
    })?;
    let with_faces = frames.iter().filter(|(_, f)| !f.is_empty()).count();
    let path = smooth(&pick_subjects(&frames));
    Ok(Track { path, frames: frames.len(), with_faces })
}

/// Move a source-time path onto the edited timeline, dropping points inside cuts.
pub fn to_output_keys(path: &[(f64, f64)], timeline: &Timeline) -> Vec<(f64, f64)> {
    let kept = timeline.kept();
    let mut keys: Vec<(f64, f64)> = Vec::new();
    for (t, c) in path {
        if let Some(o) = crate::timeline::map_time_in(&kept, *t) {
            if keys.last().is_none_or(|(lt, _)| o > *lt + 1e-6) {
                keys.push((o, *c));
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{RegionKind, Span};

    fn face(cx: f64, size: f64) -> Face {
        Face { cx, cy: 0.4, w: size, h: size, score: 0.99 }
    }

    #[test]
    fn decode_filters_by_score_and_merges_overlaps() {
        // Three candidates: two overlapping strong ones, one weak one elsewhere.
        let scores = [0.01, 0.99, 0.05, 0.95, 0.9, 0.1];
        let boxes = [0.40, 0.2, 0.60, 0.5, 0.41, 0.21, 0.61, 0.51, 0.0, 0.0, 0.1, 0.1];
        let faces = decode_detections(&scores, &boxes, 0.7);
        assert_eq!(faces.len(), 1, "{faces:?}");
        assert!((faces[0].cx - 0.5).abs() < 1e-6 && (faces[0].w - 0.2).abs() < 1e-6);
        assert!(faces[0].score > 0.98, "kept the higher-scoring box");
    }

    #[test]
    fn decode_ignores_degenerate_boxes_and_short_input() {
        assert!(decode_detections(&[0.0, 1.0], &[0.5, 0.5, 0.5, 0.5], 0.5).is_empty());
        assert!(decode_detections(&[], &[], 0.5).is_empty());
        assert!(decode_detections(&[0.0, 1.0, 0.0, 1.0], &[0.1, 0.1, 0.3, 0.3], 0.5).len() == 1);
    }

    #[test]
    fn subject_is_followed_not_swapped_for_a_bigger_face() {
        // Speaker A at 0.3 stays; a larger B appears at 0.75 and stays too.
        let frames: Vec<_> = (0..6)
            .map(|i| (i as f64 / 3.0, vec![face(0.3 + 0.01 * i as f64, 0.15), face(0.75, 0.3)]))
            .collect();
        let s = pick_subjects(&frames);
        // First sample picks the largest (B); it must then keep following B, not flip.
        assert!(s.iter().all(|(_, c)| (c.unwrap() - 0.75).abs() < 0.05), "{s:?}");
    }

    #[test]
    fn subject_switches_after_sustained_absence() {
        let mut frames = vec![(0.0, vec![face(0.3, 0.2)]), (0.33, vec![face(0.3, 0.2)])];
        for i in 0..6 {
            frames.push((0.66 + i as f64 / 3.0, vec![face(0.8, 0.2)])); // original left; someone else
        }
        let s = pick_subjects(&frames);
        assert_eq!(s[1].1, Some(0.3));
        assert_eq!(s[2].1, None, "brief absence: hold");
        assert!((s.last().unwrap().1.unwrap() - 0.8).abs() < 1e-9, "eventually follows the new person: {s:?}");
    }

    #[test]
    fn no_faces_means_no_path() {
        let s = pick_subjects(&[(0.0, vec![]), (0.3, vec![])]);
        assert!(s.iter().all(|(_, c)| c.is_none()));
        assert!(smooth(&s).is_empty());
    }

    fn series(f: impl Fn(usize) -> Option<f64>, n: usize) -> Vec<(f64, Option<f64>)> {
        (0..n).map(|i| (i as f64 / 3.0, f(i))).collect()
    }

    #[test]
    fn smoothing_ignores_jitter_and_glitches() {
        let raw = series(|i| Some(if i == 10 { 0.9 } else { 0.5 + if i % 2 == 0 { 0.01 } else { -0.01 } }), 30);
        let p = smooth(&raw);
        assert_eq!(p.len(), 30);
        assert!(p.iter().all(|(_, c)| (c - 0.5).abs() < 0.02), "steady: {p:?}");
    }

    #[test]
    fn smoothing_bridges_gaps_and_holds_the_ends() {
        let raw = series(|i| if (5..15).contains(&i) { None } else { Some(0.4) }, 20);
        assert!(smooth(&raw).iter().all(|(_, c)| (c - 0.4).abs() < 1e-9));
        let lead = series(|i| if i < 3 { None } else { Some(0.6) }, 8);
        assert!((smooth(&lead)[0].1 - 0.6).abs() < 1e-9, "start holds the first known position");
    }

    #[test]
    fn smoothing_eases_real_moves_and_snaps_on_cuts() {
        // Gradual walk right: follows, but lags slightly and never overshoots.
        let walk = series(|i| Some(0.3 + 0.01 * i as f64), 30);
        let p = smooth(&walk);
        assert!(p.last().unwrap().1 > 0.45 && p.last().unwrap().1 <= 0.59 + 1e-9, "{:?}", p.last());
        assert!(p.windows(2).all(|w| w[1].1 >= w[0].1 - 1e-12), "monotonic");
        // Scene cut: a 0.4 jump is followed immediately once confirmed by the median.
        let cut = series(|i| Some(if i < 10 { 0.25 } else { 0.7 }), 20);
        let p = smooth(&cut);
        assert!((p[19].1 - 0.7).abs() < 1e-9 && (p[0].1 - 0.25).abs() < 1e-9);
        assert!(p[12].1 > 0.6, "snapped, not glided: {}", p[12].1);
    }

    #[test]
    fn output_keys_skip_cuts_and_shift_time() {
        let mut t = Timeline::new(100.0);
        t.add(RegionKind::Manual, Span::new(0.0, 10.0), None);
        t.add(RegionKind::Manual, Span::new(40.0, 100.0), None);
        t.add(RegionKind::Silence, Span::new(20.0, 25.0), None);
        let path = vec![(5.0, 0.1), (12.0, 0.2), (22.0, 0.3), (30.0, 0.4), (50.0, 0.9)];
        let keys = to_output_keys(&path, &t);
        assert_eq!(keys, vec![(2.0, 0.2), (15.0, 0.4)], "{keys:?}");
    }

    #[test]
    fn usable_needs_enough_faces() {
        let t = Track { path: vec![(0.0, 0.5)], frames: 100, with_faces: 10 };
        assert!(!t.is_usable());
        let t = Track { path: vec![(0.0, 0.5)], frames: 100, with_faces: 60 };
        assert!(t.is_usable());
        assert!(!Track::default().is_usable());
    }
}
