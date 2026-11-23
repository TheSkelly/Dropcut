import { useEffect, useRef, useState } from "react";
import type { Region, Timeline } from "../types";
import { formatTime } from "../timeline";

interface Props {
  peaks: [number, number][];
  timeline: Timeline;
  currentTime: number;
  onSeek: (t: number) => void;
  onToggleRegion: (id: number) => void;
}

const HEIGHT = 132;
const MAX_CANVAS = 30000;

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Waveform with cut regions drawn on top. Click a region to keep/restore it. */
export function Waveform({ peaks, timeline, currentTime, onSeek, onToggleRegion }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [zoom, setZoom] = useState(1);
  const [viewWidth, setViewWidth] = useState(800);
  const [hover, setHover] = useState<Region | null>(null);

  const duration = timeline.duration || 1;
  const width = Math.min(MAX_CANVAS, Math.max(viewWidth, viewWidth * zoom));
  const pxPerSec = width / duration;

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewWidth(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Draw waveform + regions.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(width * dpr);
    canvas.height = Math.floor(HEIGHT * dpr);
    canvas.style.width = `${width}px`;
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, HEIGHT);

    const colors = {
      wave: cssVar("--wave"),
      waveCut: cssVar("--wave-cut"),
      silence: cssVar("--cut"),
      manual: cssVar("--manual"),
      profanity: cssVar("--censor"),
      off: cssVar("--off"),
    };

    const isCutAt = (t: number) =>
      timeline.regions.some((r) => r.active && r.kind !== "profanity" && t >= r.span.start && t < r.span.end);

    // Waveform, one column per pixel.
    const mid = HEIGHT / 2;
    const n = peaks.length;
    if (n > 0) {
      for (let x = 0; x < width; x++) {
        const i0 = Math.floor((x / width) * n);
        const i1 = Math.max(i0 + 1, Math.floor(((x + 1) / width) * n));
        let lo = 0;
        let hi = 0;
        for (let i = i0; i < i1 && i < n; i++) {
          lo = Math.min(lo, peaks[i][0]);
          hi = Math.max(hi, peaks[i][1]);
        }
        const t = x / pxPerSec;
        ctx.fillStyle = isCutAt(t) ? colors.waveCut : colors.wave;
        const top = mid - Math.min(1, hi * 1.6) * (mid - 6);
        const bottom = mid - Math.max(-1, lo * 1.6) * (mid - 6);
        ctx.fillRect(x, top, 1, Math.max(1, bottom - top));
      }
    }

    // Regions.
    for (const r of timeline.regions) {
      const x = r.span.start * pxPerSec;
      const w = Math.max(2, (r.span.end - r.span.start) * pxPerSec);
      const color = r.kind === "silence" ? colors.silence : r.kind === "manual" ? colors.manual : colors.profanity;
      if (r.active) {
        ctx.globalAlpha = r.kind === "profanity" ? 0.45 : 0.28;
        ctx.fillStyle = color;
        ctx.fillRect(x, 0, w, HEIGHT);
        ctx.globalAlpha = 1;
        ctx.fillRect(x, r.kind === "profanity" ? 0 : HEIGHT - 4, w, 4);
      } else {
        ctx.strokeStyle = colors.off;
        ctx.setLineDash([4, 4]);
        ctx.strokeRect(x + 0.5, 0.5, w - 1, HEIGHT - 1);
        ctx.setLineDash([]);
      }
    }
  }, [peaks, timeline, width, pxPerSec]);

  // Keep the playhead in view while playing.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || zoom <= 1) return;
    const x = currentTime * pxPerSec;
    if (x < el.scrollLeft || x > el.scrollLeft + el.clientWidth - 40) {
      el.scrollLeft = Math.max(0, x - el.clientWidth / 3);
    }
  }, [currentTime, pxPerSec, zoom]);

  const regionAt = (clientX: number): Region | null => {
    const rect = canvasRef.current!.getBoundingClientRect();
    const t = (clientX - rect.left) / pxPerSec;
    const tol = 3 / pxPerSec;
    // Profanity first: it's usually small and sits inside speech.
    const hits = timeline.regions.filter((r) => t >= r.span.start - tol && t <= r.span.end + tol);
    return hits.find((r) => r.kind === "profanity") ?? hits[0] ?? null;
  };

  const timeAt = (clientX: number) => {
    const rect = canvasRef.current!.getBoundingClientRect();
    return Math.max(0, Math.min(duration, (clientX - rect.left) / pxPerSec));
  };

  return (
    <div className="waveform">
      <div className="waveform-toolbar">
        <span className="legend">
          <i className="sw cut" /> Silence <i className="sw manual" /> Your cuts <i className="sw censor" /> Censored{" "}
          <i className="sw off" /> Kept
        </span>
        <span className="hint">
          {hover
            ? `${hover.kind === "profanity" ? `“${hover.label ?? "word"}”` : hover.kind === "silence" ? "Pause" : "Cut"} ${formatTime(
                hover.span.start,
              )}, ${(hover.span.end - hover.span.start).toFixed(2)}s. Click to ${hover.active ? "keep it" : "remove it again"}`
            : "Click a highlighted region to keep or remove it. Click elsewhere to jump there."}
        </span>
        <label className="zoom">
          Zoom
          <input type="range" min={1} max={40} step={0.5} value={zoom} onChange={(e) => setZoom(Number(e.target.value))} />
        </label>
      </div>
      <div className="waveform-scroll" ref={scrollRef}>
        <div className="waveform-inner" style={{ width }}>
          <canvas
            ref={canvasRef}
            height={HEIGHT}
            style={{ cursor: hover ? "pointer" : "text" }}
            onMouseMove={(e) => setHover(regionAt(e.clientX))}
            onMouseLeave={() => setHover(null)}
            onClick={(e) => {
              const r = regionAt(e.clientX);
              if (r) onToggleRegion(r.id);
              else onSeek(timeAt(e.clientX));
            }}
          />
          <div className="playhead" style={{ left: currentTime * pxPerSec }} />
        </div>
      </div>
    </div>
  );
}
