import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, fileName, inTauri, mediaUrl, onProgress, pickMedia, pickOutput, type Exported } from "./api";
import { Player, type PlayerHandle } from "./components/Player";
import { ShortsPanel } from "./components/ShortsPanel";
import { TranscriptView } from "./components/TranscriptView";
import { Waveform } from "./components/Waveform";
import {
  addRegion,
  censors,
  cuts,
  formatTime,
  kept,
  outputDuration,
  setKindActive,
  toggleRegion,
} from "./timeline";
import {
  defaultSettings,
  type AppInfo,
  type Cue,
  type MediaInfo,
  type Progress,
  type Settings,
  type Timeline,
  type Transcript,
  type ZoomSegment,
} from "./types";

const SETTINGS_KEY = "dropcut.settings.v1";

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (!raw) return defaultSettings;
    const s = JSON.parse(raw);
    return {
      ...defaultSettings,
      ...s,
      silence: { ...defaultSettings.silence, ...s.silence },
      profanity: { ...defaultSettings.profanity, ...s.profanity },
      captions: { ...defaultSettings.captions, ...s.captions },
      render: { ...defaultSettings.render, ...s.render },
      burn: { ...defaultSettings.burn, ...s.burn },
      zoom: { ...defaultSettings.zoom, ...s.zoom },
    };
  } catch {
    return defaultSettings;
  }
}

const LANGUAGES: [string, string][] = [
  ["auto", "Detect automatically"],
  ["en", "English"],
  ["es", "Spanish"],
  ["fr", "French"],
  ["de", "German"],
  ["pt", "Portuguese"],
  ["it", "Italian"],
  ["nl", "Dutch"],
  ["pl", "Polish"],
  ["ja", "Japanese"],
  ["ko", "Korean"],
  ["zh", "Chinese"],
];

const parseList = (s: string) =>
  s
    .split(/[,\n]/)
    .map((w) => w.trim())
    .filter(Boolean);

export default function App() {
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [info, setInfo] = useState<MediaInfo | null>(null);
  const [peaks, setPeaks] = useState<[number, number][]>([]);
  const [timeline, setTimeline] = useState<Timeline>({ duration: 0, regions: [] });
  const [threshold, setThreshold] = useState<number | null>(null);
  const [transcript, setTranscript] = useState<Transcript | null>(null);
  const [cues, setCues] = useState<Cue[]>([]);
  const [zoomSegments, setZoomSegments] = useState<ZoomSegment[]>([]);
  const [busy, setBusy] = useState<Progress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [exported, setExported] = useState<Exported | null>(null);
  const [shortsDone, setShortsDone] = useState<{ folder: string; n: number; sample: string; notes: string[] } | null>(null);
  const [previewEdits, setPreviewEdits] = useState(true);
  const [currentTime, setCurrentTime] = useState(0);
  const [dragOver, setDragOver] = useState(false);
  const player = useRef<PlayerHandle>(null);

  const words = useMemo(() => transcript?.words ?? [], [transcript]);
  const keptSpans = useMemo(() => kept(timeline), [timeline]);
  const cutSpans = useMemo(() => cuts(timeline), [timeline]);
  const censorSpans = useMemo(() => censors(timeline), [timeline]);
  const outDur = useMemo(() => outputDuration(timeline), [timeline]);

  const update = <K extends keyof Settings>(key: K, value: Settings[K]) => setSettings((s) => ({ ...s, [key]: value }));

  useEffect(() => {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch {
      /* storage unavailable: settings just won't persist */
    }
  }, [settings]);

  const refreshInfo = useCallback(() => {
    if (inTauri) api.appInfo().then(setAppInfo).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refreshInfo();
    const un = onProgress((p) => setBusy((b) => (b ? p : b)));
    return () => void un.then((f) => f());
  }, [refreshInfo]);

  const run = async <T,>(task: Progress["task"], message: string, fn: () => Promise<T>): Promise<T | undefined> => {
    setError(null);
    setBusy({ task, value: null, message });
    try {
      return await fn();
    } catch (e) {
      const msg = String(e);
      if (msg !== "cancelled") setError(msg);
      return undefined;
    } finally {
      setBusy(null);
    }
  };

  // ------------------------------------------------------------ open

  const openPath = async (path: string) => {
    const opened = await run("open", `Opening ${fileName(path)}`, () => api.openMedia(path, settings.silence));
    if (!opened) return;
    setInfo(opened.info);
    setPeaks(opened.peaks);
    setTimeline(opened.timeline);
    setThreshold(opened.threshold_db);
    setTranscript(null);
    setCues([]);
    setExported(null);
    setCurrentTime(0);
  };

  // Drag a file onto the window.
  useEffect(() => {
    if (!inTauri) return;
    let unlisten: (() => void) | undefined;
    import("@tauri-apps/api/webview").then(({ getCurrentWebview }) =>
      getCurrentWebview()
        .onDragDropEvent((e) => {
          if (e.payload.type === "over" || e.payload.type === "enter") setDragOver(true);
          else if (e.payload.type === "leave") setDragOver(false);
          else if (e.payload.type === "drop") {
            setDragOver(false);
            const p = e.payload.paths[0];
            if (p) void openPath(p);
          }
        })
        .then((f) => (unlisten = f)),
    );
    return () => unlisten?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.silence]);

  // ------------------------------------------------------------ silences

  const silenceKey = JSON.stringify(settings.silence);
  const firstSilence = useRef(true);
  useEffect(() => {
    if (firstSilence.current) {
      firstSilence.current = false;
      return;
    }
    if (!info) return;
    const id = setTimeout(() => {
      api
        .detectSilences(timeline, settings.silence)
        .then((r) => {
          setTimeline(r.timeline);
          setThreshold(r.threshold_db);
        })
        .catch((e) => setError(String(e)));
    }, 200);
    return () => clearTimeout(id);
    // Only re-run when the settings change, not on every timeline edit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [silenceKey]);

  // ------------------------------------------------------------ transcription

  const selectedModel = appInfo?.models.find((m) => m.id === settings.model);

  const transcribe = async () => {
    if (!info) return;
    if (selectedModel && !selectedModel.downloaded) {
      const ok = await run("download", `Downloading ${selectedModel.label}`, () => api.downloadModel(settings.model).then(() => true));
      refreshInfo();
      if (!ok) return;
    }
    const r = await run("transcribe", "Loading speech model", () =>
      api.transcribe({
        model: settings.model,
        language: settings.language === "auto" ? null : settings.language,
        use_gpu: settings.useGpu,
        silence: settings.silence,
        profanity: settings.profanity,
        timeline,
      }),
    );
    if (r) {
      setTranscript(r.transcript);
      setTimeline(r.timeline);
    }
  };

  // Re-scan profanity when its settings change.
  const profanityKey = JSON.stringify(settings.profanity);
  useEffect(() => {
    if (!transcript) return;
    api
      .findProfanity(timeline, transcript.words, settings.profanity)
      .then(setTimeline)
      .catch((e) => setError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [profanityKey]);

  const editWord = (i: number, text: string) => {
    if (!transcript) return;
    const next = { ...transcript, words: transcript.words.map((w, j) => (j === i ? { ...w, text } : w)) };
    setTranscript(next);
    api.findProfanity(timeline, next.words, settings.profanity).then(setTimeline).catch((e) => setError(String(e)));
  };

  // Caption preview follows every edit.
  const captionKey = JSON.stringify(settings.captions);
  useEffect(() => {
    if (!inTauri || words.length === 0) {
      setCues((c) => (c.length ? [] : c));
      return;
    }
    const id = setTimeout(() => {
      api.previewCaptions(timeline, words, settings.captions).then(setCues).catch(() => {});
    }, 120);
    return () => clearTimeout(id);
  }, [timeline, words, captionKey, settings.captions]);

  // Zoom plan follows every edit too.
  const zoomKey = JSON.stringify(settings.zoom);
  useEffect(() => {
    if (!inTauri || !info || !settings.zoom.enabled) {
      setZoomSegments((z) => (z.length ? [] : z));
      return;
    }
    const id = setTimeout(() => {
      api.previewZoom(timeline, words, settings.captions, settings.zoom).then(setZoomSegments).catch(() => {});
    }, 120);
    return () => clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [timeline, words, zoomKey, captionKey, info]);

  // ------------------------------------------------------------ export

  const doExport = async () => {
    if (!info) return;
    const output = await pickOutput(info.path, info.has_video);
    if (!output) return;
    const result = await run("export", "Starting export", () =>
      api.export({
        output,
        timeline,
        words,
        render: { ...settings.render, censor_mode: settings.profanity.mode },
        captions: settings.captions,
        write_srt: settings.writeSrt,
        burn: settings.burn,
        zoom: settings.zoom,
      }),
    );
    if (result) setExported(result);
  };

  // ------------------------------------------------------------ keyboard

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if (e.code === "Space" && info) {
        e.preventDefault();
        player.current?.toggle();
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o") {
        e.preventDefault();
        void pickMedia().then((p) => { if (p) void openPath(p); });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [info, settings.silence]);

  const seek = (t: number) => player.current?.seek(t);
  const onTime = useCallback((t: number) => setCurrentTime(t), []);

  const silenceRegions = timeline.regions.filter((r) => r.kind === "silence");
  const activeSilences = silenceRegions.filter((r) => r.active);
  const profanityRegions = timeline.regions.filter((r) => r.kind === "profanity");
  const manualRegions = timeline.regions.filter((r) => r.kind === "manual" && r.active);
  const saved = (info?.duration ?? 0) - outDur;

  // ------------------------------------------------------------ render

  return (
    <div className={`app ${dragOver ? "drag-over" : ""}`}>
      <header className="topbar">
        <div className="brand">
          <span className="logo" aria-hidden>
            <i />
            <i />
            <i />
          </span>
          DropCut
        </div>
        <button onClick={() => void pickMedia().then((p) => { if (p) void openPath(p); })} disabled={!!busy}>
          Open video…
        </button>
        {info && (
          <div className="file-meta">
            <b title={info.path}>{fileName(info.path)}</b>
            <span>
              {formatTime(info.duration)} → <strong>{formatTime(outDur)}</strong>
              {saved > 0.5 && <em> ({formatTime(saved)} shorter)</em>}
            </span>
          </div>
        )}
        <div className="spacer" />
        {info && (
          <label className="check">
            <input type="checkbox" checked={previewEdits} onChange={(e) => setPreviewEdits(e.target.checked)} />
            Preview edits
          </label>
        )}
        <button className="primary" disabled={!info || !!busy} onClick={doExport}>
          Export
        </button>
      </header>

      {error && (
        <div className="banner error" role="alert">
          <span>{error}</span>
          <button className="ghost" onClick={() => setError(null)}>
            Dismiss
          </button>
        </div>
      )}
      {appInfo && !appInfo.ffmpeg_ok && (
        <div className="banner error">ffmpeg wasn't found ({appInfo.ffmpeg_path}). Reinstall DropCut to restore it.</div>
      )}
      {exported && (
        <div className="banner ok">
          <span>
            Exported <b>{fileName(exported.output)}</b> ({formatTime(exported.duration)})
            {exported.srt && <> and captions <b>{fileName(exported.srt)}</b></>}.
          </span>
          <button className="ghost" onClick={() => api.reveal(exported.output)}>
            Show in folder
          </button>
          <button className="ghost" onClick={() => setExported(null)}>
            Close
          </button>
        </div>
      )}

      {shortsDone && (
        <div className="banner ok">
          <span>
            Exported <b>{shortsDone.n}</b> vertical short{shortsDone.n === 1 ? "" : "s"} to <b>{shortsDone.folder}</b>.
            {shortsDone.notes.map((n) => (
              <small key={n} className="muted" style={{ display: "block" }}>
                {n}
              </small>
            ))}
          </span>
          <button className="ghost" onClick={() => api.reveal(shortsDone.sample)}>
            Show in folder
          </button>
          <button className="ghost" onClick={() => setShortsDone(null)}>
            Close
          </button>
        </div>
      )}

      {!info ? (
        <main className="welcome">
          <div className="dropzone" onClick={() => void pickMedia().then((p) => { if (p) void openPath(p); })}>
            <div className="drop-icon" aria-hidden>
              ⬇
            </div>
            <h1>Drop a video here</h1>
            <p>or click to choose a file. MP4, MOV, MKV, WebM, or audio.</p>
            <ul>
              <li>Removes pauses and dead air automatically</li>
              <li>Transcribes on your own computer and makes captions you can edit</li>
              <li>Mutes or bleeps swear words</li>
            </ul>
          </div>
          {appInfo && (
            <p className="muted small">
              DropCut {appInfo.version} · Speech engine: {appInfo.gpu_backend}
            </p>
          )}
        </main>
      ) : (
        <main className="editor">
          <section className="stage">
            <Player
              ref={player}
              src={mediaUrl(info.path)}
              isVideo={info.has_video}
              keptSpans={keptSpans}
              cutSpans={cutSpans}
              censorSpans={censorSpans}
              censorMode={settings.profanity.mode}
              cues={cues}
              previewEdits={previewEdits}
              outputDuration={outDur}
              zoom={zoomSegments}
              zoomFocusY={settings.zoom.focus_y}
              burn={settings.burn}
              onTime={onTime}
            />
            <Waveform
              peaks={peaks}
              timeline={timeline}
              currentTime={currentTime}
              onSeek={seek}
              onToggleRegion={(id) => setTimeline((t) => toggleRegion(t, id))}
            />
            <div className="panel transcript-panel">
              <div className="panel-head">
                <h2>Transcript</h2>
                {transcript && (
                  <span className="muted small">
                    {words.length} words{transcript.language ? ` · ${transcript.language}` : ""} · click to jump, shift-click to
                    select, Delete to cut, double-click to fix a word
                  </span>
                )}
              </div>
              <TranscriptView
                words={words}
                cutSpans={cutSpans}
                censorSpans={censorSpans}
                currentTime={currentTime}
                onSeek={seek}
                onCut={(span, label) => setTimeline((t) => addRegion(t, "manual", span, label))}
                onEditWord={editWord}
              />
            </div>
          </section>

          <aside className="sidebar">
            <section className="panel">
              <div className="panel-head">
                <h2>Silences</h2>
                <span className="badge">{activeSilences.length} removed</span>
              </div>
              <label className="row">
                <span>Detection</span>
                <select
                  value={settings.silence.threshold_db === null ? "auto" : "manual"}
                  onChange={(e) =>
                    update("silence", {
                      ...settings.silence,
                      threshold_db: e.target.value === "auto" ? null : Math.round(threshold ?? -40),
                    })
                  }
                >
                  <option value="auto">Automatic</option>
                  <option value="manual">Manual threshold</option>
                </select>
              </label>
              {settings.silence.threshold_db !== null ? (
                <Slider
                  label="Threshold"
                  value={settings.silence.threshold_db}
                  min={-70}
                  max={-15}
                  step={1}
                  format={(v) => `${v} dB`}
                  hint="Raise it if background noise isn't being cut; lower it if quiet words get cut."
                  onChange={(v) => update("silence", { ...settings.silence, threshold_db: v })}
                />
              ) : (
                threshold !== null && <p className="muted small">Using {threshold.toFixed(0)} dB, based on this recording.</p>
              )}
              <Slider
                label="Shortest pause to cut"
                value={settings.silence.min_silence}
                min={0.15}
                max={2}
                step={0.05}
                format={(v) => `${v.toFixed(2)}s`}
                onChange={(v) => update("silence", { ...settings.silence, min_silence: v })}
              />
              <Slider
                label="Breathing room"
                value={(settings.silence.pad_before + settings.silence.pad_after) / 2}
                min={0}
                max={0.5}
                step={0.01}
                format={(v) => `${v.toFixed(2)}s`}
                hint="Audio kept around each word so cuts don't feel clipped."
                onChange={(v) => update("silence", { ...settings.silence, pad_before: v * 0.8, pad_after: v * 1.2 })}
              />
              <div className="btn-row">
                <button className="ghost" onClick={() => setTimeline((t) => setKindActive(t, "silence", true))}>
                  Cut all
                </button>
                <button className="ghost" onClick={() => setTimeline((t) => setKindActive(t, "silence", false))}>
                  Keep all
                </button>
              </div>
              {manualRegions.length > 0 && (
                <p className="muted small">
                  Plus {manualRegions.length} cut{manualRegions.length > 1 ? "s" : ""} you made in the transcript.{" "}
                  <button className="link" onClick={() => setTimeline((t) => ({ ...t, regions: t.regions.filter((r) => r.kind !== "manual") }))}>
                    Undo all
                  </button>
                </p>
              )}
            </section>

            <section className="panel">
              <div className="panel-head">
                <h2>Captions</h2>
                {transcript && <span className="badge">{cues.length} captions</span>}
              </div>
              <label className="row">
                <span>Speech model</span>
                <select value={settings.model} onChange={(e) => update("model", e.target.value)}>
                  {(appInfo?.models ?? []).map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.label} · {m.size_mb >= 1000 ? `${(m.size_mb / 1000).toFixed(1)} GB` : `${m.size_mb} MB`}
                      {m.downloaded ? " ✓" : ""}
                    </option>
                  ))}
                </select>
              </label>
              {selectedModel && <p className="muted small">{selectedModel.note}</p>}
              <label className="row">
                <span>Language</span>
                <select value={settings.language} onChange={(e) => update("language", e.target.value)}>
                  {LANGUAGES.map(([code, name]) => (
                    <option key={code} value={code}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="check">
                <input type="checkbox" checked={settings.useGpu} onChange={(e) => update("useGpu", e.target.checked)} />
                Use GPU ({appInfo?.gpu_backend ?? "…"})
              </label>
              <button className="primary wide" disabled={!!busy} onClick={transcribe}>
                {transcript ? "Transcribe again" : selectedModel && !selectedModel.downloaded ? "Download model & transcribe" : "Transcribe"}
              </button>
              <label className="check">
                <input
                  type="checkbox"
                  checked={settings.burn.enabled}
                  onChange={(e) => update("burn", { ...settings.burn, enabled: e.target.checked })}
                />
                Burn animated captions into the video
              </label>
              {settings.burn.enabled && (
                <>
                  <div className="style-grid" role="radiogroup" aria-label="Caption style">
                    {(
                      [
                        ["clean", "Clean", "Whole line, white"],
                        ["highlight", "Highlight", "Spoken word lights up"],
                        ["pop", "Pop", "Bold, punchy, word pops"],
                        ["karaoke", "Karaoke", "Words fill as spoken"],
                      ] as const
                    ).map(([id, name, hint]) => (
                      <button
                        key={id}
                        role="radio"
                        aria-checked={settings.burn.style === id}
                        className={`style-card ${settings.burn.style === id ? "on" : ""}`}
                        onClick={() => update("burn", { ...settings.burn, style: id })}
                      >
                        <b className={`sample sample-${id}`}>{id === "pop" ? "WORD" : "Word"}</b>
                        <span>{name}</span>
                        <small>{hint}</small>
                      </button>
                    ))}
                  </div>
                  <label className="row">
                    <span>Position</span>
                    <select
                      value={settings.burn.position}
                      onChange={(e) => update("burn", { ...settings.burn, position: e.target.value as Settings["burn"]["position"] })}
                    >
                      <option value="raised">Lower third (best for Shorts/Reels)</option>
                      <option value="bottom">Bottom</option>
                      <option value="center">Center</option>
                    </select>
                  </label>
                  <Slider
                    label="Caption size"
                    value={settings.burn.size}
                    min={0.6}
                    max={1.8}
                    step={0.05}
                    format={(v) => `${Math.round(v * 100)}%`}
                    onChange={(v) => update("burn", { ...settings.burn, size: v })}
                  />
                  <label className="row">
                    <span>Highlight</span>
                    <input
                      type="color"
                      value={settings.burn.highlight}
                      onChange={(e) => update("burn", { ...settings.burn, highlight: e.target.value.toUpperCase() })}
                    />
                  </label>
                </>
              )}
              <Slider
                label="Words per caption"
                value={settings.captions.max_words}
                min={1}
                max={12}
                step={1}
                format={(v) => `${v}`}
                onChange={(v) => update("captions", { ...settings.captions, max_words: v, max_chars: Math.max(12, v * 7) })}
              />
              <label className="check">
                <input
                  type="checkbox"
                  checked={settings.captions.uppercase}
                  onChange={(e) => update("captions", { ...settings.captions, uppercase: e.target.checked })}
                />
                UPPERCASE captions
              </label>
              <label className="check">
                <input type="checkbox" checked={settings.writeSrt} onChange={(e) => update("writeSrt", e.target.checked)} />
                Save an .srt caption file with the export
              </label>
            </section>

            <section className="panel">
              <div className="panel-head">
                <h2>Auto zoom</h2>
                {settings.zoom.enabled && <span className="badge">{zoomSegments.length} punch-ins</span>}
              </div>
              <label className="check">
                <input
                  type="checkbox"
                  checked={settings.zoom.enabled}
                  onChange={(e) => update("zoom", { ...settings.zoom, enabled: e.target.checked })}
                />
                Punch in automatically
              </label>
              {settings.zoom.enabled && (
                <>
                  <div className="segmented" role="radiogroup" aria-label="When to zoom">
                    {(
                      [
                        ["on_cuts", "At cuts"],
                        ["on_sentences", "At sentences"],
                      ] as const
                    ).map(([id, name]) => (
                      <button
                        key={id}
                        role="radio"
                        aria-checked={settings.zoom.mode === id}
                        className={settings.zoom.mode === id ? "on" : ""}
                        onClick={() => update("zoom", { ...settings.zoom, mode: id })}
                      >
                        {name}
                      </button>
                    ))}
                  </div>
                  {settings.zoom.mode === "on_sentences" && !transcript && (
                    <p className="muted small">Transcribe first so DropCut knows where sentences start.</p>
                  )}
                  <Slider
                    label="Zoom amount"
                    value={settings.zoom.amount}
                    min={1.05}
                    max={1.5}
                    step={0.01}
                    format={(v) => `${Math.round((v - 1) * 100)}% closer`}
                    onChange={(v) => update("zoom", { ...settings.zoom, amount: v })}
                  />
                  <Slider
                    label="Hold each framing at least"
                    value={settings.zoom.min_hold}
                    min={0.8}
                    max={8}
                    step={0.1}
                    format={(v) => `${v.toFixed(1)}s`}
                    onChange={(v) => update("zoom", { ...settings.zoom, min_hold: v })}
                  />
                  <Slider
                    label="Zoom toward"
                    value={settings.zoom.focus_y}
                    min={0.2}
                    max={0.7}
                    step={0.01}
                    format={(v) => (v < 0.35 ? "upper frame" : v > 0.55 ? "lower frame" : "face height")}
                    hint="Where faces usually sit in your shots."
                    onChange={(v) => update("zoom", { ...settings.zoom, focus_y: v })}
                  />
                </>
              )}
            </section>

            <section className="panel">
              <div className="panel-head">
                <h2>Profanity</h2>
                {transcript && <span className="badge">{profanityRegions.filter((r) => r.active).length} censored</span>}
              </div>
              <label className="check">
                <input
                  type="checkbox"
                  checked={settings.profanity.enabled}
                  onChange={(e) => update("profanity", { ...settings.profanity, enabled: e.target.checked })}
                />
                Censor swear words
              </label>
              <div className="segmented" role="radiogroup" aria-label="Censor style">
                {(["mute", "bleep"] as const).map((m) => (
                  <button
                    key={m}
                    role="radio"
                    aria-checked={settings.profanity.mode === m}
                    className={settings.profanity.mode === m ? "on" : ""}
                    onClick={() => update("profanity", { ...settings.profanity, mode: m })}
                  >
                    {m === "mute" ? "Mute" : "Bleep"}
                  </button>
                ))}
              </div>
              <ListInput
                label="Also censor"
                placeholder="e.g. frick, heck"
                value={settings.profanity.extra_words}
                onChange={(v) => update("profanity", { ...settings.profanity, extra_words: v })}
              />
              <ListInput
                label="Allow these"
                placeholder="e.g. damn, crap"
                value={settings.profanity.allowed_words}
                onChange={(v) => update("profanity", { ...settings.profanity, allowed_words: v })}
              />
              {!transcript && <p className="muted small">Swear words are found after you transcribe.</p>}
              {profanityRegions.length > 0 && (
                <ul className="chips">
                  {profanityRegions.map((r) => (
                    <li key={r.id}>
                      <button
                        className={r.active ? "chip on" : "chip"}
                        title={r.active ? "Click to leave this one uncensored" : "Click to censor"}
                        onClick={() => setTimeline((t) => toggleRegion(t, r.id))}
                        onDoubleClick={() => seek(r.span.start)}
                      >
                        {r.label} <small>{formatTime(r.span.start)}</small>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>

            {info && (
              <ShortsPanel
                info={info}
                words={words}
                settings={settings}
                busy={!!busy}
                run={run}
                onSeek={seek}
                onDone={(folder, n, sample, notes) => setShortsDone({ folder, n, sample, notes })}
              />
            )}

            <section className="panel">
              <div className="panel-head">
                <h2>Export</h2>
              </div>
              <label className="row">
                <span>Encoder</span>
                <select
                  value={settings.render.encoder}
                  onChange={(e) => update("render", { ...settings.render, encoder: e.target.value as Settings["render"]["encoder"] })}
                >
                  <option value="auto">Automatic (GPU if available)</option>
                  <option value="nvenc">NVIDIA GPU</option>
                  <option value="video_toolbox">Apple hardware</option>
                  <option value="x264">CPU (most compatible)</option>
                </select>
              </label>
              <label className="row">
                <span>Quality</span>
                <select
                  value={settings.render.quality}
                  onChange={(e) => update("render", { ...settings.render, quality: e.target.value as Settings["render"]["quality"] })}
                >
                  <option value="draft">Draft (fast, smaller)</option>
                  <option value="high">High</option>
                  <option value="max">Maximum</option>
                </select>
              </label>
              <button className="primary wide" disabled={!!busy} onClick={doExport}>
                Export {formatTime(outDur)} video
              </button>
            </section>
          </aside>
        </main>
      )}

      {busy && (
        <div className="busy" role="status">
          <div className="busy-text">{busy.message}</div>
          <div className={`bar ${busy.value === null ? "indeterminate" : ""}`}>
            <div style={{ width: `${Math.round((busy.value ?? 0) * 100)}%` }} />
          </div>
          {busy.task !== "open" && (
            <button className="ghost" onClick={() => api.cancel()}>
              Cancel
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function Slider(props: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  format: (v: number) => string;
  hint?: string;
  onChange: (v: number) => void;
}) {
  return (
    <label className="slider">
      <span className="slider-head">
        <span>{props.label}</span>
        <output>{props.format(props.value)}</output>
      </span>
      <input
        type="range"
        min={props.min}
        max={props.max}
        step={props.step}
        value={props.value}
        onChange={(e) => props.onChange(Number(e.target.value))}
      />
      {props.hint && <small className="muted">{props.hint}</small>}
    </label>
  );
}

function ListInput(props: { label: string; placeholder: string; value: string[]; onChange: (v: string[]) => void }) {
  const [text, setText] = useState(props.value.join(", "));
  useEffect(() => setText(props.value.join(", ")), [props.value]);
  return (
    <label className="row stacked">
      <span>{props.label}</span>
      <input
        type="text"
        value={text}
        placeholder={props.placeholder}
        onChange={(e) => setText(e.target.value)}
        onBlur={() => props.onChange(parseList(text))}
        onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
      />
    </label>
  );
}
