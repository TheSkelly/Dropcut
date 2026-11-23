// Typed wrappers around the Tauri commands in src-tauri/src/lib.rs.

import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  AppInfo,
  CaptionSettings,
  Cue,
  MediaInfo,
  ProfanitySettings,
  Progress,
  RenderSettings,
  SilenceSettings,
  Timeline,
  Transcript,
  VideoEncoder,
  Word,
} from "./types";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface Opened {
  info: MediaInfo;
  peaks: [number, number][];
  threshold_db: number;
  timeline: Timeline;
}

export interface Exported {
  output: string;
  srt: string | null;
  encoder: VideoEncoder;
  duration: number;
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  cancel: () => invoke<void>("cancel"),
  openMedia: (path: string, silence: SilenceSettings) => invoke<Opened>("open_media", { path, silence }),
  detectSilences: (timeline: Timeline, silence: SilenceSettings) =>
    invoke<{ threshold_db: number; timeline: Timeline }>("detect_silences", { timeline, silence }),
  downloadModel: (id: string) => invoke<void>("download_model", { id }),
  transcribe: (req: {
    model: string;
    language: string | null;
    use_gpu: boolean;
    silence: SilenceSettings;
    profanity: ProfanitySettings;
    timeline: Timeline;
  }) => invoke<{ transcript: Transcript; timeline: Timeline }>("transcribe", { req }),
  findProfanity: (timeline: Timeline, words: Word[], profanity: ProfanitySettings) =>
    invoke<Timeline>("find_profanity", { timeline, words, profanity }),
  export: (req: {
    output: string;
    timeline: Timeline;
    words: Word[];
    render: RenderSettings;
    captions: CaptionSettings;
    write_srt: boolean;
  }) => invoke<Exported>("export", { req }),
  previewCaptions: (timeline: Timeline, words: Word[], captions: CaptionSettings) =>
    invoke<Cue[]>("preview_captions", { timeline, words, captions }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
};

export function onProgress(cb: (p: Progress) => void): Promise<UnlistenFn> {
  if (!inTauri) return Promise.resolve(() => {});
  return listen<Progress>("progress", (e) => cb(e.payload));
}

export const VIDEO_EXTENSIONS = ["mp4", "mov", "mkv", "m4v", "webm", "avi", "mp3", "wav", "m4a", "aac", "flac"];

export async function pickMedia(): Promise<string | null> {
  const picked = await open({ multiple: false, filters: [{ name: "Video or audio", extensions: VIDEO_EXTENSIONS }] });
  return typeof picked === "string" ? picked : null;
}

export async function pickOutput(source: string, hasVideo: boolean): Promise<string | null> {
  const dot = source.lastIndexOf(".");
  const base = dot > 0 ? source.slice(0, dot) : source;
  const ext = hasVideo ? "mp4" : "m4a";
  return save({
    defaultPath: `${base} (DropCut).${ext}`,
    filters: [hasVideo ? { name: "MP4 video", extensions: ["mp4"] } : { name: "Audio", extensions: ["m4a"] }],
  });
}

export function mediaUrl(path: string): string {
  return inTauri ? convertFileSrc(path) : path;
}

export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}
