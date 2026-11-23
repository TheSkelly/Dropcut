//! DropCut desktop app: thin Tauri layer over `dropcut-core`.
//!
//! The React UI owns the editable state (timeline, transcript, settings) and
//! sends it with each command; the backend only caches decoded audio so
//! re-running silence detection with new settings is instant.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dropcut_core::captions::{self, CaptionSettings};
use dropcut_core::media::{self, FfmpegPaths, MediaInfo};
use dropcut_core::profanity::{self, ProfanitySettings};
use dropcut_core::render::{self, RenderSettings};
use dropcut_core::silence::{self, SilenceSettings};
use dropcut_core::transcript::{self, TranscribeOptions, Transcript};
use dropcut_core::{models, RegionKind, Timeline};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

/// Decoded audio for the currently open file.
struct Loaded {
    path: PathBuf,
    samples: Arc<Vec<f32>>,
    levels: Arc<Vec<f32>>,
}

#[derive(Default)]
struct AppState {
    loaded: Mutex<Option<Loaded>>,
    cancel: Arc<AtomicBool>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn ffmpeg_paths() -> FfmpegPaths {
    // Installers place the ffmpeg sidecars next to the app executable.
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    FfmpegPaths::from_dir_or_path(exe_dir.as_deref())
}

fn models_dir(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app.path().app_data_dir().map_err(err)?.join("models"))
}

#[derive(Clone, Serialize)]
struct Progress {
    task: &'static str,
    /// 0..1, or null when unknown.
    value: Option<f64>,
    message: String,
}

fn emit(app: &AppHandle, task: &'static str, value: Option<f64>, message: impl Into<String>) {
    let _ = app.emit("progress", Progress { task, value, message: message.into() });
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?
}

// ---------------------------------------------------------------- app info

#[derive(Serialize)]
struct ModelEntry {
    id: &'static str,
    label: &'static str,
    size_mb: u32,
    note: &'static str,
    downloaded: bool,
}

#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    gpu_backend: &'static str,
    ffmpeg_ok: bool,
    ffmpeg_path: String,
    models: Vec<ModelEntry>,
    default_model: &'static str,
}

fn gpu_backend() -> &'static str {
    if cfg!(feature = "cuda") {
        "NVIDIA CUDA"
    } else if cfg!(feature = "vulkan") {
        "Vulkan (NVIDIA / AMD / Intel)"
    } else if cfg!(feature = "metal") {
        "Apple Metal"
    } else {
        "CPU only"
    }
}

#[tauri::command]
fn app_info(app: AppHandle) -> CmdResult<AppInfo> {
    let dir = models_dir(&app)?;
    let paths = ffmpeg_paths();
    let ffmpeg_ok = paths.command_ok();
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        gpu_backend: gpu_backend(),
        ffmpeg_ok,
        ffmpeg_path: paths.ffmpeg.display().to_string(),
        models: models::MODELS
            .iter()
            .map(|m| ModelEntry {
                id: m.id,
                label: m.label,
                size_mb: m.size_mb,
                note: m.note,
                downloaded: models::is_downloaded(&dir, m.id),
            })
            .collect(),
        default_model: models::DEFAULT_MODEL,
    })
}

#[tauri::command]
fn cancel(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::Relaxed);
}

fn reset_cancel(state: &AppState) -> Arc<AtomicBool> {
    state.cancel.store(false, Ordering::Relaxed);
    state.cancel.clone()
}

// ---------------------------------------------------------------- open + silences

#[derive(Serialize)]
struct Opened {
    info: MediaInfo,
    /// [min, max] pairs for drawing the waveform.
    peaks: Vec<[f32; 2]>,
    threshold_db: f32,
    timeline: Timeline,
}

#[tauri::command]
async fn open_media(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    silence: SilenceSettings,
) -> CmdResult<Opened> {
    let paths = ffmpeg_paths();
    emit(&app, "open", None, "Reading file");
    let p = path.clone();
    let (info, samples) = blocking(move || {
        let info = media::probe(&paths, &p).map_err(err)?;
        if !info.has_audio {
            return Err("This file has no audio track, so there is nothing to cut or caption.".into());
        }
        let samples = media::decode_audio(&paths, &p).map_err(err)?;
        Ok((info, samples))
    })
    .await?;

    let levels = silence::frame_levels_db(&samples, media::ANALYSIS_RATE);
    let peaks = silence::waveform_peaks(&samples, 4000);
    let analysis = silence::detect_from_levels(&levels, &silence);
    let mut timeline = Timeline::new(info.duration);
    timeline.replace_kind(RegionKind::Silence, analysis.silences.iter().map(|s| (*s, None)));

    *state.loaded.lock().unwrap() = Some(Loaded { path, samples: Arc::new(samples), levels: Arc::new(levels) });
    emit(&app, "open", Some(1.0), "Ready");
    Ok(Opened { info, peaks, threshold_db: analysis.threshold_db, timeline })
}

#[derive(Serialize)]
struct Detected {
    threshold_db: f32,
    timeline: Timeline,
}

/// Re-run silence detection with new settings, keeping every other region.
#[tauri::command]
fn detect_silences(state: State<'_, AppState>, timeline: Timeline, silence: SilenceSettings) -> CmdResult<Detected> {
    let guard = state.loaded.lock().unwrap();
    let loaded = guard.as_ref().ok_or("No file is open.")?;
    let analysis = silence::detect_from_levels(&loaded.levels, &silence);
    let mut timeline = timeline;
    timeline.replace_kind(RegionKind::Silence, analysis.silences.iter().map(|s| (*s, None)));
    Ok(Detected { threshold_db: analysis.threshold_db, timeline })
}

// ---------------------------------------------------------------- models + transcription

#[tauri::command]
async fn download_model(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let dir = models_dir(&app)?;
    let cancel = reset_cancel(&state);
    let label = models::find(&id).map(|m| m.label).unwrap_or("model");
    let a = app.clone();
    blocking(move || {
        let mut last = -1i64;
        models::download(
            &dir,
            &id,
            |done, total| {
                let pct = total.map(|t| done as f64 / t.max(1) as f64);
                let step = pct.map(|p| (p * 200.0) as i64).unwrap_or((done >> 22) as i64);
                if step != last {
                    last = step;
                    emit(&a, "download", pct, format!("Downloading {label}: {} MB", done / 1_000_000));
                }
            },
            Some(cancel),
        )
        .map(|_| ())
        .map_err(err)
    })
    .await
}

#[derive(Deserialize)]
struct TranscribeRequest {
    model: String,
    language: Option<String>,
    use_gpu: bool,
    silence: SilenceSettings,
    profanity: ProfanitySettings,
    timeline: Timeline,
}

#[derive(Serialize)]
struct Transcribed {
    transcript: Transcript,
    timeline: Timeline,
}

#[tauri::command]
async fn transcribe(app: AppHandle, state: State<'_, AppState>, req: TranscribeRequest) -> CmdResult<Transcribed> {
    let model = models::path_in(&models_dir(&app)?, &req.model);
    if !model.is_file() {
        return Err("That speech model isn't downloaded yet.".into());
    }
    let (samples, levels) = {
        let guard = state.loaded.lock().unwrap();
        let l = guard.as_ref().ok_or("No file is open.")?;
        (l.samples.clone(), l.levels.clone())
    };
    let cancel = reset_cancel(&state);
    let a = app.clone();
    let opts = TranscribeOptions {
        language: req.language.filter(|l| !l.is_empty() && l != "auto"),
        use_gpu: req.use_gpu,
        ..Default::default()
    };
    emit(&app, "transcribe", Some(0.0), "Loading speech model");

    let mut transcript = blocking(move || {
        let progress = move |p: i32| emit(&a, "transcribe", Some(p as f64 / 100.0), format!("Transcribing {p}%"));
        transcript::transcribe(&model, &samples, &opts, progress, Some(cancel)).map_err(err)
    })
    .await?;

    let threshold = req.silence.threshold_db.unwrap_or_else(|| silence::auto_threshold(&levels));
    transcript::align_to_speech(&mut transcript.words, &silence::speech_spans(&levels, threshold, 0.25));

    let mut timeline = req.timeline;
    timeline.replace_kind(RegionKind::Profanity, profanity::find(&transcript.words, &req.profanity));
    emit(&app, "transcribe", Some(1.0), format!("{} words", transcript.words.len()));
    Ok(Transcribed { transcript, timeline })
}

/// Re-scan for swear words after the user edits the word list or transcript.
#[tauri::command]
fn find_profanity(timeline: Timeline, words: Vec<transcript::Word>, profanity: ProfanitySettings) -> Timeline {
    let mut timeline = timeline;
    timeline.replace_kind(RegionKind::Profanity, profanity::find(&words, &profanity));
    timeline
}

// ---------------------------------------------------------------- export

#[derive(Deserialize)]
struct ExportRequest {
    output: PathBuf,
    timeline: Timeline,
    words: Vec<transcript::Word>,
    render: RenderSettings,
    captions: CaptionSettings,
    write_srt: bool,
}

#[derive(Serialize)]
struct Exported {
    output: PathBuf,
    srt: Option<PathBuf>,
    encoder: render::VideoEncoder,
    duration: f64,
}

#[tauri::command]
async fn export(app: AppHandle, state: State<'_, AppState>, req: ExportRequest) -> CmdResult<Exported> {
    let source = {
        let guard = state.loaded.lock().unwrap();
        guard.as_ref().ok_or("No file is open.")?.path.clone()
    };
    if source == req.output {
        return Err("Choose a different file name; exporting over the original isn't allowed.".into());
    }
    let cancel = reset_cancel(&state);
    let a = app.clone();
    blocking(move || {
        let paths = ffmpeg_paths();
        let info = media::probe(&paths, &source).map_err(err)?;
        emit(&a, "export", Some(0.0), "Starting export");
        let outcome = render::render(
            &paths,
            &info,
            &req.timeline,
            &req.output,
            &req.render,
            |p| emit(&a, "export", Some(p), format!("Exporting {:.0}%", p * 100.0)),
            Some(cancel),
        )
        .map_err(err)?;

        let srt = if req.write_srt && !req.words.is_empty() {
            let srt_path = req.output.with_extension("srt");
            let cues = captions::build_cues(&req.words, &req.timeline, &req.captions);
            std::fs::write(&srt_path, captions::to_srt(&cues)).map_err(err)?;
            Some(srt_path)
        } else {
            None
        };
        emit(&a, "export", Some(1.0), "Done");
        Ok(Exported { output: outcome.output, srt, encoder: outcome.encoder, duration: outcome.duration })
    })
    .await
}

/// Captions for the current edit, for previewing in the UI.
#[tauri::command]
fn preview_captions(timeline: Timeline, words: Vec<transcript::Word>, captions: CaptionSettings) -> Vec<captions::Cue> {
    captions::build_cues(&words, &timeline, &captions)
}

#[tauri::command]
fn reveal(path: PathBuf) -> CmdResult<()> {
    let target = path.parent().unwrap_or(&path).to_path_buf();
    let opener = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(target).spawn().map(|_| ()).map_err(err)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            app_info,
            cancel,
            open_media,
            detect_silences,
            download_model,
            transcribe,
            find_profanity,
            export,
            preview_captions,
            reveal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DropCut");
}
