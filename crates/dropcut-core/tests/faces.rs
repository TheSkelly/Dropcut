//! The real face model against a real photo (NASA astronaut portrait, public
//! domain). Reference position comes from running the same model in
//! onnxruntime: face box about x 179..269, y 63..173 on a 512 px image.
#![cfg(feature = "faces")]

use std::path::Path;
use std::process::Command;

use dropcut_core::media::FfmpegPaths;
use dropcut_core::reframe::{FaceDetector, UltraFace, DET_H, DET_W};

fn rgb_frame(paths: &FfmpegPaths, image: &Path) -> Option<Vec<u8>> {
    let out = Command::new(&paths.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(image)
        .args(["-vf", &format!("scale={DET_W}:{DET_H},format=rgb24"), "-frames:v", "1", "-f", "rawvideo", "pipe:1"])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

#[test]
fn finds_the_face_where_onnxruntime_does() {
    let paths = FfmpegPaths::default();
    if !paths.command_ok() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let img = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/astronaut.jpg");
    let frame = rgb_frame(&paths, &img).expect("decode fixture");
    assert_eq!(frame.len(), DET_W * DET_H * 3);

    let mut det = UltraFace::new().expect("model loads");
    let faces = det.detect(&frame).unwrap();
    assert!(!faces.is_empty(), "no face found");
    let best = faces.iter().max_by(|a, b| a.score.total_cmp(&b.score)).unwrap();
    assert!(best.score > 0.9, "{best:?}");
    // Reference: centre (224, 118) of 512 => (0.4375, 0.230).
    assert!((best.cx - 0.4375).abs() < 0.03, "cx {} vs 0.4375", best.cx);
    assert!((best.cy - 0.230).abs() < 0.04, "cy {} vs 0.230", best.cy);
}

#[test]
fn a_frame_with_no_face_finds_nothing() {
    let mut det = UltraFace::new().unwrap();
    let blank = vec![128u8; DET_W * DET_H * 3];
    assert!(det.detect(&blank).unwrap().is_empty());
    assert!(det.detect(&blank[..10]).is_err(), "wrong-sized frame is an error, not a panic");
}
