//! `dropcut` command-line tool: the same engine as the desktop app, handy for
//! batch jobs and for testing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dropcut_core::burn::{self, BurnSettings, CaptionStyle};
use dropcut_core::captions::{self, CaptionSettings};
use dropcut_core::media::{self, FfmpegPaths};
use dropcut_core::profanity::{self, CensorMode, ProfanitySettings};
use dropcut_core::render::{self, RenderSettings, VideoEffects, VideoEncoder};
use dropcut_core::silence::{self, SilenceSettings};
use dropcut_core::transcript::{self, TranscribeOptions, Transcript};
use dropcut_core::zoom::{self, ZoomSettings};
use dropcut_core::{models, RegionKind, Timeline};

const USAGE: &str = "\
DropCut: remove silences, caption and censor videos.

USAGE:
  dropcut cut <input> -o <output> [options]   Edit and export
  dropcut analyze <input> [options]           Show what would be cut (JSON)
  dropcut transcribe <input> [-o words.json]  Word-level transcript
  dropcut models                              List speech models
  dropcut download <model>                    Download a speech model

OPTIONS:
  -o, --output <path>      Output file
  --srt <path>             Also write captions (SRT) matching the edited video
  --model <id|path>        Speech model (default: large-v3-turbo-q5_0)
  --language <code>        Spoken language, e.g. en (default: auto-detect)
  --threshold <dB>         Silence threshold, e.g. -40 (default: automatic)
  --min-silence <secs>     Shortest pause to remove (default 0.5)
  --padding <secs>         Audio kept around speech (default 0.15)
  --keep-silences          Don't remove silences
  --no-captions            Skip speech recognition entirely
  --no-profanity           Don't censor swear words
  --bleep                  Bleep swear words instead of muting
  --encoder <name>         auto | x264 | nvenc | videotoolbox
  --cpu                    Don't use the GPU for speech recognition
  --burn <style>           Burn animated captions into the video:
                           clean | highlight | pop | karaoke
  --fonts <dir>            Caption font folder (default: next to dropcut)
  --zoom [amount]          Automatic punch-in zooms (default 1.15)
";

#[derive(Default)]
struct Args {
    command: String,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    srt: Option<PathBuf>,
    model: Option<String>,
    language: Option<String>,
    silence: SilenceSettings,
    keep_silences: bool,
    no_captions: bool,
    profanity: ProfanitySettings,
    encoder: VideoEncoder,
    cpu: bool,
    burn: Option<CaptionStyle>,
    fonts: Option<PathBuf>,
    zoom: Option<f64>,
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1).peekable();
    let mut a = Args { command: raw.next().unwrap_or_default(), ..Default::default() };
    let mut positional = Vec::new();
    while let Some(arg) = raw.next() {
        let mut value = |name: &str| raw.next().ok_or_else(|| format!("{name} needs a value"));
        let num = |v: String, name: &str| v.parse::<f64>().map_err(|_| format!("{name}: not a number: {v}"));
        match arg.as_str() {
            "-o" | "--output" => a.output = Some(value(&arg)?.into()),
            "--srt" => a.srt = Some(value(&arg)?.into()),
            "--model" => a.model = Some(value(&arg)?),
            "--language" => a.language = Some(value(&arg)?),
            "--threshold" => a.silence.threshold_db = Some(num(value(&arg)?, &arg)? as f32),
            "--min-silence" => a.silence.min_silence = num(value(&arg)?, &arg)?,
            "--padding" => {
                let p = num(value(&arg)?, &arg)?;
                a.silence.pad_before = p;
                a.silence.pad_after = p;
            }
            "--keep-silences" => a.keep_silences = true,
            "--no-captions" => a.no_captions = true,
            "--no-profanity" => a.profanity.enabled = false,
            "--bleep" => a.profanity.mode = CensorMode::Bleep,
            "--cpu" => a.cpu = true,
            "--burn" => {
                a.burn = Some(match value(&arg)?.as_str() {
                    "clean" => CaptionStyle::Clean,
                    "highlight" => CaptionStyle::Highlight,
                    "pop" => CaptionStyle::Pop,
                    "karaoke" => CaptionStyle::Karaoke,
                    other => return Err(format!("unknown caption style {other}")),
                })
            }
            "--fonts" => a.fonts = Some(value(&arg)?.into()),
            "--zoom" => {
                // Optional numeric amount right after the flag.
                let next = raw.peek().cloned();
                match next.as_deref().and_then(|v| v.parse::<f64>().ok()) {
                    Some(v) => {
                        raw.next();
                        a.zoom = Some(v);
                    }
                    None => a.zoom = Some(1.15),
                }
            }
            "--encoder" => {
                a.encoder = match value(&arg)?.as_str() {
                    "auto" => VideoEncoder::Auto,
                    "x264" | "cpu" => VideoEncoder::X264,
                    "nvenc" => VideoEncoder::Nvenc,
                    "videotoolbox" => VideoEncoder::VideoToolbox,
                    other => return Err(format!("unknown encoder {other}")),
                }
            }
            "-h" | "--help" => a.command = "help".into(),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => positional.push(arg),
        }
    }
    a.input = positional.into_iter().next().map(PathBuf::from);
    Ok(a)
}

fn models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("DROPCUT_MODELS") {
        return d.into();
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default();
    Path::new(&home).join(".dropcut").join("models")
}

/// Accepts either a catalogue id (downloaded on demand) or a path to a model file.
fn resolve_model(spec: Option<&str>) -> Result<PathBuf, String> {
    let spec = spec.unwrap_or(models::DEFAULT_MODEL);
    let as_path = Path::new(spec);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }
    let dir = models_dir();
    if !models::is_downloaded(&dir, spec) {
        download_model(spec)?;
    }
    Ok(models::path_in(&dir, spec))
}

#[cfg(feature = "whisper")]
fn download_model(id: &str) -> Result<(), String> {
    eprintln!("Downloading speech model {id} to {} ...", models_dir().display());
    let mut last = 0;
    models::download(
        &models_dir(),
        id,
        |done, total| {
            if let Some(t) = total {
                let pct = done * 100 / t.max(1);
                if pct != last {
                    last = pct;
                    eprint!("\r  {pct}%");
                }
            }
        },
        None,
    )
    .map_err(|e| e.to_string())?;
    eprintln!();
    Ok(())
}

#[cfg(not(feature = "whisper"))]
fn download_model(_id: &str) -> Result<(), String> {
    Err("this build has no speech recognition; rebuild with --features whisper".into())
}

fn transcribe_file(samples: &[f32], a: &Args) -> Result<Transcript, String> {
    let model = resolve_model(a.model.as_deref())?;
    let opts = TranscribeOptions { language: a.language.clone(), use_gpu: !a.cpu, ..Default::default() };
    eprintln!("Transcribing with {} ...", model.display());
    let t = transcript::transcribe(&model, samples, &opts, |p| eprint!("\r  {p}%"), None).map_err(|e| e.to_string())?;
    eprintln!("\r  done, {} words ({})", t.words.len(), t.language.as_deref().unwrap_or("?"));
    Ok(t)
}

fn run(a: Args) -> Result<(), String> {
    let paths = FfmpegPaths::from_dir_or_path(std::env::current_exe().ok().as_deref().and_then(Path::parent));
    match a.command.as_str() {
        "models" => {
            let dir = models_dir();
            for m in models::MODELS {
                let mark = if models::is_downloaded(&dir, m.id) { "downloaded" } else { "" };
                println!("{:<22} {:>5} MB  {:<11} {}", m.id, m.size_mb, mark, m.note);
            }
            Ok(())
        }
        "download" => {
            let id = a.input.as_ref().ok_or("which model?")?.to_string_lossy().to_string();
            download_model(&id)
        }
        "analyze" | "transcribe" | "cut" => {
            let input = a.input.clone().ok_or("missing input file")?;
            let info = media::probe(&paths, &input).map_err(|e| e.to_string())?;
            if !info.has_audio {
                return Err("this file has no audio track".into());
            }
            eprintln!("Decoding audio ({:.1}s) ...", info.duration);
            let samples = media::decode_audio(&paths, &input).map_err(|e| e.to_string())?;

            if a.command == "transcribe" {
                let levels = silence::frame_levels_db(&samples, media::ANALYSIS_RATE);
                let threshold = a.silence.threshold_db.unwrap_or_else(|| silence::auto_threshold(&levels));
                let mut t = transcribe_file(&samples, &a)?;
                if std::env::var_os("DROPCUT_NO_ALIGN").is_none() {
                    transcript::align_to_speech(&mut t.words, &silence::speech_spans(&levels, threshold, 0.25));
                }
                let json = serde_json::to_string_pretty(&t).unwrap();
                match &a.output {
                    Some(p) => std::fs::write(p, json).map_err(|e| e.to_string())?,
                    None => println!("{json}"),
                }
                return Ok(());
            }

            let mut timeline = Timeline::new(info.duration);
            let levels = silence::frame_levels_db(&samples, media::ANALYSIS_RATE);
            let analysis = silence::detect_from_levels(&levels, &a.silence);
            if !a.keep_silences {
                timeline.replace_kind(RegionKind::Silence, analysis.silences.iter().map(|s| (*s, None)));
            }

            let transcript = if a.no_captions {
                None
            } else {
                let mut t = transcribe_file(&samples, &a)?;
                transcript::align_to_speech(&mut t.words, &silence::speech_spans(&levels, analysis.threshold_db, 0.25));
                Some(t)
            };
            if let Some(t) = &transcript {
                timeline.replace_kind(RegionKind::Profanity, profanity::find(&t.words, &a.profanity));
            }

            eprintln!(
                "Threshold {:.1} dB: {} silences, {} censored words. {:.1}s -> {:.1}s",
                analysis.threshold_db,
                timeline.regions.iter().filter(|r| r.kind == RegionKind::Silence).count(),
                timeline.regions.iter().filter(|r| r.kind == RegionKind::Profanity).count(),
                info.duration,
                timeline.output_duration()
            );

            if a.command == "analyze" {
                println!("{}", serde_json::to_string_pretty(&timeline).unwrap());
                return Ok(());
            }

            let output = a.output.clone().ok_or("missing -o <output>")?;
            let settings = RenderSettings { encoder: a.encoder, censor_mode: a.profanity.mode, ..Default::default() };
            let caption_settings = CaptionSettings::default();
            let out_words = transcript.as_ref().map(|t| captions::output_words(&t.words, &timeline, &caption_settings)).unwrap_or_default();
            let mut effects = VideoEffects { zoom_focus_y: 0.4, ..Default::default() };
            if let Some(amount) = a.zoom {
                let zs = ZoomSettings { enabled: true, amount, ..Default::default() };
                effects.zoom = zoom::plan(&timeline, &out_words, &zs);
                eprintln!("{} punch-in zooms", effects.zoom.len());
            }
            if let (Some(style), Some(t)) = (a.burn, &transcript) {
                let cues = captions::build_cues(&t.words, &timeline, &caption_settings);
                let bs = BurnSettings { enabled: true, style, ..Default::default() };
                effects.subtitles = Some(burn::to_ass(&cues, &bs, info.width.unwrap_or(1920), info.height.unwrap_or(1080)));
                effects.fonts_dir = a.fonts.clone().or_else(|| {
                    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("fonts"))).filter(|d| d.is_dir())
                });
            }
            let outcome = render::render(&paths, &info, &timeline, &output, &settings, &effects, |p| eprint!("\r  rendering {:>3.0}%", p * 100.0), None)
                .map_err(|e| e.to_string())?;
            eprintln!("\nWrote {} ({:?})", outcome.output.display(), outcome.encoder);

            if let (Some(srt), Some(t)) = (&a.srt, &transcript) {
                let cues = captions::build_cues(&t.words, &timeline, &CaptionSettings::default());
                std::fs::write(srt, captions::to_srt(&cues)).map_err(|e| e.to_string())?;
                eprintln!("Wrote {} ({} captions)", srt.display(), cues.len());
            }
            Ok(())
        }
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match parse_args().and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
