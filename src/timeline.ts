// Client-side timeline helpers, matching dropcut-core's timeline.rs so the
// preview behaves exactly like the export.

import type { Region, RegionKind, Span, Timeline } from "./types";

export function merge(spans: Span[]): Span[] {
  const sorted = spans.filter((s) => s.end > s.start).sort((a, b) => a.start - b.start);
  const out: Span[] = [];
  for (const s of sorted) {
    const last = out[out.length - 1];
    if (last && s.start <= last.end) last.end = Math.max(last.end, s.end);
    else out.push({ ...s });
  }
  return out;
}

const isCut = (k: RegionKind) => k === "silence" || k === "manual";

export function cuts(t: Timeline): Span[] {
  return merge(t.regions.filter((r) => r.active && isCut(r.kind)).map((r) => r.span));
}

export function censors(t: Timeline): Span[] {
  return merge(t.regions.filter((r) => r.active && r.kind === "profanity").map((r) => r.span));
}

export function kept(t: Timeline): Span[] {
  const out: Span[] = [];
  let cursor = 0;
  for (const c of cuts(t)) {
    if (c.start > cursor) out.push({ start: cursor, end: c.start });
    cursor = Math.max(cursor, c.end);
  }
  if (cursor < t.duration) out.push({ start: cursor, end: t.duration });
  return out.filter((s) => s.end - s.start >= 0.02);
}

export function outputDuration(t: Timeline): number {
  return kept(t).reduce((sum, s) => sum + (s.end - s.start), 0);
}

/** Source time -> output time, or null if that moment was cut. */
export function mapTime(keptSpans: Span[], t: number): number | null {
  let offset = 0;
  for (const s of keptSpans) {
    if (t < s.start) return null;
    if (t <= s.end) return offset + (t - s.start);
    offset += s.end - s.start;
  }
  return null;
}

/** The active cut containing `t`, if any. */
export function cutAt(cutSpans: Span[], t: number): Span | undefined {
  return cutSpans.find((c) => t >= c.start && t < c.end - 0.01);
}

export function toggleRegion(t: Timeline, id: number): Timeline {
  return { ...t, regions: t.regions.map((r) => (r.id === id ? { ...r, active: !r.active } : r)) };
}

export function setKindActive(t: Timeline, kind: RegionKind, active: boolean): Timeline {
  return { ...t, regions: t.regions.map((r) => (r.kind === kind ? { ...r, active } : r)) };
}

export function addRegion(t: Timeline, kind: RegionKind, span: Span, label?: string): Timeline {
  const id = t.regions.reduce((m, r) => Math.max(m, r.id + 1), 1);
  const region: Region = { id, kind, span, active: true, label };
  const regions = [...t.regions, region].sort((a, b) => a.span.start - b.span.start);
  return { ...t, regions };
}

export function removeRegion(t: Timeline, id: number): Timeline {
  return { ...t, regions: t.regions.filter((r) => r.id !== id) };
}

export function formatTime(secs: number): string {
  if (!isFinite(secs)) return "0:00";
  const s = Math.max(0, secs);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = Math.floor(s % 60);
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

export function formatPrecise(secs: number): string {
  const tenths = Math.floor((secs % 1) * 10);
  return `${formatTime(secs)}.${tenths}`;
}
