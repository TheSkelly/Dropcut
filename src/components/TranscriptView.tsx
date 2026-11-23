import { useEffect, useMemo, useRef, useState } from "react";
import type { Span, Word } from "../types";

interface Props {
  words: Word[];
  cutSpans: Span[];
  censorSpans: Span[];
  currentTime: number;
  onSeek: (t: number) => void;
  /** Remove (cut) the given source range. */
  onCut: (span: Span, label: string) => void;
  onEditWord: (index: number, text: string) => void;
}

const coveredBy = (spans: Span[], w: Word, frac: number) => {
  const len = Math.max(1e-6, w.end - w.start);
  const covered = spans.reduce((sum, s) => sum + Math.max(0, Math.min(s.end, w.end) - Math.max(s.start, w.start)), 0);
  return covered >= len * frac;
};

/**
 * Text-based editing: click a word to jump there, shift-click to select a
 * range, press Delete to cut it, double-click to fix a misheard word.
 */
export function TranscriptView({ words, cutSpans, censorSpans, currentTime, onSeek, onCut, onEditWord }: Props) {
  const [sel, setSel] = useState<[number, number] | null>(null);
  const [editing, setEditing] = useState<number | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const activeRef = useRef<HTMLSpanElement>(null);

  const flags = useMemo(
    () => words.map((w) => ({ cut: coveredBy(cutSpans, w, 0.7), censored: coveredBy(censorSpans, w, 0.6) })),
    [words, cutSpans, censorSpans],
  );

  const current = useMemo(() => {
    let lo = 0;
    let hi = words.length - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (words[mid].start <= currentTime) {
        found = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    return found >= 0 && currentTime <= words[found].end + 0.3 ? found : -1;
  }, [words, currentTime]);

  useEffect(() => {
    const el = activeRef.current;
    const box = containerRef.current;
    if (!el || !box) return;
    const top = el.offsetTop - box.offsetTop;
    if (top < box.scrollTop || top > box.scrollTop + box.clientHeight - 30) {
      box.scrollTo({ top: top - box.clientHeight / 3, behavior: "smooth" });
    }
  }, [current]);

  const cutSelection = () => {
    if (!sel) return;
    const [a, b] = sel[0] <= sel[1] ? sel : [sel[1], sel[0]];
    const span = { start: words[a].start, end: words[b].end };
    const label = words
      .slice(a, b + 1)
      .map((w) => w.text)
      .join(" ");
    onCut(span, label.length > 40 ? `${label.slice(0, 40)}…` : label);
    setSel(null);
  };

  const inSel = (i: number) => sel !== null && i >= Math.min(sel[0], sel[1]) && i <= Math.max(sel[0], sel[1]);

  if (words.length === 0) {
    return (
      <div className="transcript empty">
        <p>No transcript yet. Click <b>Transcribe</b> to get captions, catch swear words and edit by text.</p>
      </div>
    );
  }

  return (
    <div
      className="transcript"
      ref={containerRef}
      tabIndex={0}
      onKeyDown={(e) => {
        if (editing !== null) return;
        if ((e.key === "Delete" || e.key === "Backspace") && sel) {
          e.preventDefault();
          cutSelection();
        } else if (e.key === "Escape") setSel(null);
      }}
    >
      {words.map((w, i) => {
        const f = flags[i];
        const cls = [
          "word",
          f.cut && "is-cut",
          f.censored && "is-censored",
          i === current && "is-current",
          inSel(i) && "is-selected",
          w.confidence < 0.5 && "is-unsure",
        ]
          .filter(Boolean)
          .join(" ");
        if (editing === i) {
          return (
            <input
              key={i}
              className="word-edit"
              autoFocus
              defaultValue={w.text}
              size={Math.max(3, w.text.length + 1)}
              onBlur={(e) => {
                const v = e.target.value.trim();
                if (v && v !== w.text) onEditWord(i, v);
                setEditing(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                if (e.key === "Escape") setEditing(null);
              }}
            />
          );
        }
        return (
          <span
            key={i}
            ref={i === current ? activeRef : undefined}
            className={cls}
            title={w.confidence < 0.5 ? "The model wasn't sure about this word. Double-click to fix it." : undefined}
            onClick={(e) => {
              if (e.shiftKey && sel) setSel([sel[0], i]);
              else {
                setSel([i, i]);
                onSeek(w.start);
              }
            }}
            onDoubleClick={() => setEditing(i)}
          >
            {w.text}{" "}
          </span>
        );
      })}
      {sel && (
        <div className="selection-bar">
          <span>
            {Math.abs(sel[1] - sel[0]) + 1} word{sel[0] === sel[1] ? "" : "s"} selected
          </span>
          <button className="danger" onClick={cutSelection}>
            Cut selection (Delete)
          </button>
          <button className="ghost" onClick={() => setSel(null)}>
            Clear
          </button>
        </div>
      )}
    </div>
  );
}
