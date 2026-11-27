import { useEffect, useState } from "react";
import { api, fileName, joinPath, pickFolder } from "../api";
import { formatTime } from "../timeline";
import type { MediaInfo, Progress, Settings, ShortCandidate, Word } from "../types";

type Run = <T>(task: Progress["task"], message: string, fn: () => Promise<T>) => Promise<T | undefined>;

interface Props {
  info: MediaInfo;
  words: Word[];
  settings: Settings;
  busy: boolean;
  run: Run;
  onSeek: (t: number) => void;
  onDone: (folder: string, count: number, sampleFile: string, notes: string[]) => void;
}

const KEY_STORAGE = "dropcut.anthropicKey";

function slug(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "short";
}

export function ShortsPanel({ info, words, settings, busy, run, onSeek, onDone }: Props) {
  const [count, setCount] = useState(5);
  const [minLen, setMinLen] = useState(20);
  const [maxLen, setMaxLen] = useState(75);
  const [focusX, setFocusX] = useState(0.5);
  const [follow, setFollow] = useState(true);
  const [tighten, setTighten] = useState(true);
  const [apiKey, setApiKey] = useState("");
  const [remember, setRemember] = useState(false);
  const [picks, setPicks] = useState<ShortCandidate[]>([]);
  const [chosen, setChosen] = useState<Set<number>>(new Set());
  const [note, setNote] = useState<string | null>(null);
  const [usedLlm, setUsedLlm] = useState<boolean | null>(null);

  useEffect(() => {
    try {
      const k = localStorage.getItem(KEY_STORAGE);
      if (k) {
        setApiKey(k);
        setRemember(true);
      }
    } catch {
      /* storage unavailable */
    }
  }, []);

  // A different file or transcript invalidates the old suggestions.
  useEffect(() => {
    setPicks([]);
    setChosen(new Set());
    setNote(null);
    setUsedLlm(null);
  }, [info.path, words]);

  const persistKey = () => {
    try {
      if (remember && apiKey.trim()) localStorage.setItem(KEY_STORAGE, apiKey.trim());
      else localStorage.removeItem(KEY_STORAGE);
    } catch {
      /* storage unavailable */
    }
  };

  const find = async () => {
    persistKey();
    const r = await run("export", "Finding the best moments", () =>
      api.findShorts({
        words,
        duration: info.duration,
        settings: { count, min_seconds: minLen, max_seconds: Math.max(maxLen, minLen + 5) },
        api_key: apiKey.trim() || null,
        llm_model: null,
      }),
    );
    if (!r) return;
    setPicks(r.shorts);
    setChosen(new Set(r.shorts.map((_, i) => i)));
    setNote(r.note);
    setUsedLlm(r.used_llm);
  };

  const exportChosen = async () => {
    const folder = await pickFolder();
    if (!folder) return;
    const stem = fileName(info.path).replace(/\.[^.]+$/, "");
    const todo = picks.map((p, i) => ({ p, i })).filter(({ i }) => chosen.has(i));
    let done = 0;
    let sample = "";
    const notes = new Set<string>();
    for (const { p, i } of todo) {
      const name = `${stem}_${String(i + 1).padStart(2, "0")}_${slug(p.title)}.mp4`;
      const r = await run("export", `Exporting short ${done + 1} of ${todo.length}`, () =>
        api.exportShort({
          output: joinPath(folder, name),
          pick: p,
          words,
          render: { ...settings.render, censor_mode: settings.profanity.mode },
          burn: { ...settings.burn, enabled: true },
          silence: settings.silence,
          profanity: settings.profanity,
          tighten,
          focus_x: focusX,
          follow_face: follow,
        }),
      );
      if (!r) return; // cancelled or failed; the error banner explains
      done += 1;
      sample = r.output;
      if (r.note) notes.add(r.note);
    }
    onDone(folder, done, sample, [...notes]);
  };

  const noVideo = !info.has_video;
  const noWords = words.length === 0;

  return (
    <section className="panel">
      <div className="panel-head">
        <h2>Shorts</h2>
        {usedLlm !== null && <span className="badge">{usedLlm ? "picked by Claude" : "offline picker"}</span>}
      </div>
      {noVideo && <p className="muted">Shorts need a video file.</p>}
      {!noVideo && noWords && <p className="muted">Transcribe the video first; moments are picked from what is said.</p>}
      {!noVideo && !noWords && (
        <>
          <label className="row">
            <span>Clips</span>
            <input type="number" min={1} max={20} value={count} onChange={(e) => setCount(Math.max(1, Math.min(20, Number(e.target.value) || 1)))} />
          </label>
          <label className="row">
            <span>Length (seconds)</span>
            <span className="inline">
              <input type="number" min={5} max={170} value={minLen} onChange={(e) => setMinLen(Number(e.target.value) || 20)} />
              to
              <input type="number" min={10} max={180} value={maxLen} onChange={(e) => setMaxLen(Number(e.target.value) || 75)} />
            </span>
          </label>
          <label className="check">
            <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
            Follow the speaker's face
          </label>
          <label className="slider">
            <span className="slider-head">
              <span>{follow ? "Fallback crop position" : "Crop position"}</span>
              <output>{focusX < 0.4 ? "left" : focusX > 0.6 ? "right" : "centre"}</output>
            </span>
            <input type="range" min={0} max={1} step={0.05} value={focusX} onChange={(e) => setFocusX(Number(e.target.value))} />
            <small className="muted">
              {follow
                ? "Used only when no face is found."
                : "Where the 9:16 frame sits on a wide video. Put the speaker in it."}
            </small>
          </label>
          <label className="check">
            <input type="checkbox" checked={tighten} onChange={(e) => setTighten(e.target.checked)} />
            Remove pauses inside each clip
          </label>
          <label className="row stacked">
            <span>Anthropic API key (optional)</span>
            <input
              type="password"
              autoComplete="off"
              value={apiKey}
              placeholder="Blank = free offline picker"
              onChange={(e) => setApiKey(e.target.value)}
            />
            <small className="muted">
              With a key, the transcript text is sent to Anthropic to choose the best moments. Nothing else leaves your computer.
            </small>
          </label>
          <label className="check">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
            Remember the key on this computer (stored unencrypted)
          </label>
          <button className="primary wide" disabled={busy} onClick={find}>
            {picks.length ? "Find again" : "Find the best moments"}
          </button>
          {note && <p className="muted">{note}</p>}

          {picks.length > 0 && (
            <>
              <ul className="shorts-list">
                {picks.map((p, i) => (
                  <li key={`${p.start}-${p.end}`}>
                    <label className="check">
                      <input
                        type="checkbox"
                        checked={chosen.has(i)}
                        onChange={(e) =>
                          setChosen((c) => {
                            const n = new Set(c);
                            if (e.target.checked) n.add(i);
                            else n.delete(i);
                            return n;
                          })
                        }
                      />
                      <span className="short-title">{p.title || "Untitled"}</span>
                      <span className="badge">{p.score}</span>
                    </label>
                    <div className="short-meta">
                      <button className="ghost" onClick={() => onSeek(p.start)} title="Jump to this moment in the player">
                        {formatTime(p.start)} to {formatTime(p.end)} ({Math.round(p.end - p.start)}s)
                      </button>
                      {p.hook && <small className="muted">"{p.hook}"</small>}
                    </div>
                  </li>
                ))}
              </ul>
              <button className="primary wide" disabled={busy || chosen.size === 0} onClick={exportChosen}>
                Export {chosen.size} vertical short{chosen.size === 1 ? "" : "s"}
              </button>
            </>
          )}
        </>
      )}
    </section>
  );
}
