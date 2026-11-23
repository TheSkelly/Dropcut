// Mirrors the serde types in dropcut-core. Keep in sync with the Rust side.

export interface Span {
  start: number;
  end: number;
}

export type RegionKind = "silence" | "manual" | "profanity";

export interface Region {
  id: number;
  kind: RegionKind;
  span: Span;
  active: boolean;
  label?: string;
}

export interface Timeline {
  duration: number;
  regions: Region[];
}

export interface Word {
  text: string;
  start: number;
  end: number;
  confidence: number;
}

export interface Transcript {
  language: string | null;
  words: Word[];
}

export interface MediaInfo {
  path: string;
  duration: number;
  has_video: boolean;
  has_audio: boolean;
  width: number | null;
  height: number | null;
  fps: number | null;
}

export interface SilenceSettings {
  threshold_db: number | null;
  min_silence: number;
  pad_before: number;
  pad_after: number;
}

export type CensorMode = "mute" | "bleep";

export interface ProfanitySettings {
  enabled: boolean;
  mode: CensorMode;
  extra_words: string[];
  allowed_words: string[];
  padding: number;
}

export interface CaptionSettings {
  max_chars: number;
  max_words: number;
  max_duration: number;
  break_on_gap: number;
  uppercase: boolean;
  mask_profanity: boolean;
}

export type VideoEncoder = "auto" | "x264" | "nvenc" | "video_toolbox";
export type Quality = "draft" | "high" | "max";

export interface RenderSettings {
  encoder: VideoEncoder;
  quality: Quality;
  censor_mode: CensorMode;
}

export interface CueWord {
  text: string;
  start: number;
  end: number;
}

export interface Cue {
  start: number;
  end: number;
  text: string;
  words: CueWord[];
}

export interface ModelEntry {
  id: string;
  label: string;
  size_mb: number;
  note: string;
  downloaded: boolean;
}

export interface AppInfo {
  version: string;
  gpu_backend: string;
  ffmpeg_ok: boolean;
  ffmpeg_path: string;
  models: ModelEntry[];
  default_model: string;
}

export interface Progress {
  task: "open" | "download" | "transcribe" | "export";
  value: number | null;
  message: string;
}

export interface Settings {
  silence: SilenceSettings;
  profanity: ProfanitySettings;
  captions: CaptionSettings;
  render: RenderSettings;
  model: string;
  language: string;
  useGpu: boolean;
  writeSrt: boolean;
}

export const defaultSettings: Settings = {
  silence: { threshold_db: null, min_silence: 0.5, pad_before: 0.12, pad_after: 0.18 },
  profanity: { enabled: true, mode: "mute", extra_words: [], allowed_words: [], padding: 0.04 },
  captions: { max_chars: 32, max_words: 6, max_duration: 3, break_on_gap: 0.6, uppercase: false, mask_profanity: true },
  render: { encoder: "auto", quality: "high", censor_mode: "mute" },
  model: "large-v3-turbo-q5_0",
  language: "auto",
  useGpu: true,
  writeSrt: true,
};
