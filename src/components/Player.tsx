import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import type { BurnSettings, CensorMode, Cue, Span, ZoomSegment } from "../types";
import { cutAt, formatTime, mapTime } from "../timeline";

export interface PlayerHandle {
  seek: (t: number) => void;
  toggle: () => void;
}

interface Props {
  src: string;
  isVideo: boolean;
  keptSpans: Span[];
  cutSpans: Span[];
  censorSpans: Span[];
  censorMode: CensorMode;
  cues: Cue[];
  previewEdits: boolean;
  outputDuration: number;
  zoom: ZoomSegment[];
  zoomFocusY: number;
  burn: BurnSettings;
  onTime: (t: number) => void;
}

/**
 * Plays the source file but behaves like the export: jumps over cuts,
 * mutes (or bleeps) censored words, and overlays the captions.
 */
export const Player = forwardRef<PlayerHandle, Props>(function Player(
  {
    src,
    isVideo,
    keptSpans,
    cutSpans,
    censorSpans,
    censorMode,
    cues,
    previewEdits,
    outputDuration,
    zoom,
    zoomFocusY,
    burn,
    onTime,
  },
  ref,
) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  const bleep = useRef<{ ctx: AudioContext; gain: GainNode } | null>(null);
  // Latest props for the animation loop without restarting it.
  const live = useRef({ cutSpans, censorSpans, censorMode, previewEdits });
  live.current = { cutSpans, censorSpans, censorMode, previewEdits };

  useImperativeHandle(ref, () => ({
    seek: (t: number) => {
      if (videoRef.current) videoRef.current.currentTime = t;
      setTime(t);
      onTime(t);
    },
    toggle: () => {
      const v = videoRef.current;
      if (v) void (v.paused ? v.play() : v.pause());
    },
  }));

  const ensureBleep = () => {
    if (bleep.current) return bleep.current;
    const ctx = new AudioContext();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.frequency.value = 1000;
    gain.gain.value = 0;
    osc.connect(gain).connect(ctx.destination);
    osc.start();
    bleep.current = { ctx, gain };
    return bleep.current;
  };

  useEffect(() => {
    let raf = 0;
    let lastReported = -1;
    const tick = () => {
      const v = videoRef.current;
      if (v) {
        const { cutSpans, censorSpans, censorMode, previewEdits } = live.current;
        let t = v.currentTime;
        if (previewEdits && !v.paused) {
          const cut = cutAt(cutSpans, t);
          if (cut) {
            v.currentTime = Math.min(cut.end, v.duration || cut.end);
            t = cut.end;
          }
        }
        const censored = previewEdits && censorSpans.some((s) => t >= s.start && t < s.end);
        v.muted = censored;
        if (censorMode === "bleep" && (censored || bleep.current)) {
          const b = ensureBleep();
          b.gain.gain.value = censored && !v.paused ? 0.08 : 0;
        }
        // ~25 updates a second is plenty for the playhead and captions and
        // keeps long transcripts from re-rendering every frame.
        if (Math.abs(t - lastReported) > 0.04) {
          lastReported = t;
          setTime(t);
          onTime(t);
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [onTime]);

  useEffect(() => () => void bleep.current?.ctx.close(), []);

  const outTime = useMemo(() => mapTime(keptSpans, time), [keptSpans, time]);
  const scale = useMemo(() => {
    if (!previewEdits || outTime === null) return 1;
    return zoom.find((z) => outTime >= z.start && outTime < z.end)?.scale ?? 1;
  }, [zoom, outTime, previewEdits]);

  const cue = useMemo(() => {
    if (outTime === null) return null;
    return cues.find((c) => outTime >= c.start && outTime < c.end) ?? null;
  }, [cues, outTime]);

  return (
    <div className="player">
      <div className={`screen ${isVideo ? "" : "audio-only"}`} onClick={() => videoRef.current && (videoRef.current.paused ? videoRef.current.play() : videoRef.current.pause())}>
        <video
          ref={videoRef}
          src={src}
          style={{ transform: `scale(${scale})`, transformOrigin: `50% ${zoomFocusY * 100}%` }}
          preload="auto"
          onPlay={() => setPlaying(true)}
          onPause={() => setPlaying(false)}
          playsInline
        />
        {!isVideo && <div className="audio-badge">Audio file</div>}
        {cue && (
          <div
            className={`caption-overlay style-${burn.enabled ? burn.style : "preview"} pos-${burn.enabled ? burn.position : "bottom"}`}
            style={{ ["--hl" as string]: burn.highlight, ["--cap-scale" as string]: burn.enabled ? burn.size : 0.8 }}
          >
            {cue.words.map((w, i) => {
              const next = cue.words[i + 1]?.start ?? cue.end;
              const state =
                outTime === null ? "" : outTime >= w.start && outTime < next ? "current" : outTime >= w.start ? "spoken" : "";
              return (
                <span key={i} className={state}>
                  {burn.enabled && burn.style === "pop" ? w.text.toUpperCase() : w.text}{" "}
                </span>
              );
            })}
          </div>
        )}
      </div>
      <div className="transport">
        <button className="play" onClick={() => videoRef.current && (playing ? videoRef.current.pause() : videoRef.current.play())}>
          {playing ? "Pause" : "Play"}
        </button>
        <span className="times">
          {formatTime(time)} <small>source</small>
          {previewEdits && outTime !== null && (
            <>
              {"  ·  "}
              {formatTime(outTime)} / {formatTime(outputDuration)} <small>edited</small>
            </>
          )}
        </span>
      </div>
    </div>
  );
});
