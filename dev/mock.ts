// Browser-only harness: runs the real UI with Tauri IPC mocked from a
// fixture produced by `cargo run --example ui_fixture`. For development and
// screenshots only; never bundled into the app.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";

const fixture = await (await fetch("./fixture.json")).json();
mockWindows("main");

let downloaded = false;
mockIPC(async (cmd, args: any) => {
  switch (cmd) {
    case "app_info":
      return {
        version: "0.1.0",
        gpu_backend: "Vulkan (NVIDIA / AMD / Intel)",
        ffmpeg_ok: true,
        ffmpeg_path: "ffmpeg",
        default_model: "large-v3-turbo-q5_0",
        models: [
          { id: "base", label: "Base", size_mb: 142, note: "Fast, fine for clear audio. Good on CPU.", downloaded: true },
          { id: "large-v3-turbo-q5_0", label: "Large v3 Turbo", size_mb: 547, note: "Best accuracy. Recommended with an NVIDIA GPU or Apple Silicon.", downloaded },
        ],
      };
    case "open_media":
      return fixture.opened;
    case "detect_silences":
      return { threshold_db: fixture.opened.threshold_db, timeline: args.timeline };
    case "download_model":
      downloaded = true;
      return null;
    case "transcribe":
      return fixture.transcribed;
    case "find_profanity":
      return args.timeline;
    case "preview_captions":
      return fixture.cues;
    case "preview_zoom":
      return args.zoom.enabled ? [{ start: 2.9, end: 6.0, scale: args.zoom.amount }] : [];
    case "export":
      return { output: "/videos/test (DropCut).mp4", srt: "/videos/test (DropCut).srt", encoder: "nvenc", duration: 9.8 };
    case "find_shorts":
      return {
        shorts: [
          { start: 12.4, end: 47.9, title: "Why most people quit in week two", hook: "Why does everyone quit so early?", score: 92, source: "llm" },
          { start: 80.0, end: 118.5, title: "The biggest editing mistake", hook: "Here is the mistake I made for years.", score: 81, source: "llm" },
          { start: 160.2, end: 190.0, title: "A free trick nobody mentions", hook: "Nobody tells you this part.", score: 74, source: "llm" },
        ],
        used_llm: !!args.req.api_key,
        note: args.req.api_key ? null : "Showing the offline picker (no API key).",
      };
    case "export_short":
      return { output: args.req.output, srt: null, encoder: "x264", duration: 35.5 };
    case "plugin:dialog|open":
      return "/videos/test.mp4";
    case "plugin:event|listen":
      return 1;
    default:
      return null;
  }
});
(window as any).__TAURI_INTERNALS__.convertFileSrc = () => "./test.mp4";

await import("../src/main");
