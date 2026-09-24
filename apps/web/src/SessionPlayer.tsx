import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { api, type SessionAudioIndex } from "./api";
import { playbackTimeForCapture } from "./sessionPlayback";

const SEGMENT_SECONDS = 45;
const PREFETCH_SECONDS = 15;

function clock(seconds: number): string {
  if (!Number.isFinite(seconds)) return "00:00";
  const whole = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor(whole / 60) % 60;
  const rest = whole % 60;
  return hours ? `${hours}:${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`
    : `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}

function segmentUrl(sessionId: string, startSeconds: number): string {
  return `/api/v1/sessions/${sessionId}/audio/segment?start_ms=${Math.round(startSeconds * 1000)}&duration_ms=${SEGMENT_SECONDS * 1000}`;
}

export function SessionPlayer({ sessionId, sessionState, seekRequest, onReady, live = false, refreshKey = "" }: {
  sessionId: string;
  sessionState: string;
  seekRequest: { capturedAtMs: number; serial: number } | null;
  onReady: (ready: boolean) => void;
  live?: boolean;
  refreshKey?: string;
}) {
  const audio = useRef<HTMLAudioElement>(null);
  const indexedSessionId = useRef(sessionId);
  const lastSeekSerial = useRef(0);
  const resumeAfterLoad = useRef(false);
  const pendingOffset = useRef(0);
  const pendingAbsoluteSeek = useRef<number | null>(null);
  const positionRef = useRef(0);
  const segmentStartRef = useRef(0);
  const activeBlobUrl = useRef<string | null>(null);
  const prefetched = useRef<{ start: number; url: string } | null>(null);
  const prefetchingStart = useRef<number | null>(null);
  const [index, setIndex] = useState<SessionAudioIndex | null>(null);
  const [position, setPosition] = useState(0);
  const [segmentStart, setSegmentStart] = useState(0);
  const [source, setSource] = useState(() => segmentUrl(sessionId, 0));
  const [speed, setSpeed] = useState(1);
  const [playing, setPlaying] = useState(false);
  const [buffering, setBuffering] = useState(false);
  const [error, setError] = useState("");

  const releasePrefetch = useCallback(() => {
    if (prefetched.current) URL.revokeObjectURL(prefetched.current.url);
    prefetched.current = null;
    prefetchingStart.current = null;
  }, []);

  const selectSegment = useCallback((start: number, offset: number, resume: boolean) => {
    if (activeBlobUrl.current) {
      URL.revokeObjectURL(activeBlobUrl.current);
      activeBlobUrl.current = null;
    }
    const cached = prefetched.current;
    if (cached && cached.start === start) {
      activeBlobUrl.current = cached.url;
      prefetched.current = null;
      setSource(cached.url);
    } else {
      releasePrefetch();
      setSource(segmentUrl(sessionId, start));
    }
    pendingOffset.current = offset;
    pendingAbsoluteSeek.current = start + offset;
    resumeAfterLoad.current = resume;
    segmentStartRef.current = start;
    setSegmentStart(start);
    setBuffering(resume);
    setError("");
  }, [releasePrefetch, sessionId]);

  useEffect(() => {
    let active = true;
    const sessionChanged = indexedSessionId.current !== sessionId;
    if (sessionChanged) {
      indexedSessionId.current = sessionId;
      lastSeekSerial.current = 0;
      setIndex(null);
      positionRef.current = 0;
      setPosition(0);
      segmentStartRef.current = 0;
      setSegmentStart(0);
      setSource(segmentUrl(sessionId, 0));
      pendingOffset.current = 0;
      pendingAbsoluteSeek.current = 0;
      resumeAfterLoad.current = false;
      setPlaying(false);
      setBuffering(false);
      setError("");
      if (activeBlobUrl.current) URL.revokeObjectURL(activeBlobUrl.current);
      activeBlobUrl.current = null;
      releasePrefetch();
    }
    api.sessionAudioIndex(sessionId).then((next) => {
      if (active) setIndex(next);
    }).catch(() => {
      if (active && sessionChanged) setIndex(null);
    });
    return () => { active = false; };
  }, [sessionId, sessionState, refreshKey, releasePrefetch]);

  useEffect(() => { onReady(Boolean(index)); }, [index, onReady]);

  useEffect(() => () => {
    if (activeBlobUrl.current) URL.revokeObjectURL(activeBlobUrl.current);
    releasePrefetch();
  }, [releasePrefetch]);

  const seek = useCallback((requested: number, resume = playing) => {
    if (!index || !audio.current) return;
    const duration = index.duration_ms / 1000;
    const next = Math.max(0, Math.min(duration, requested));
    // The exact recording endpoint belongs to the final playable segment, not
    // to a new empty segment whose start is equal to the duration.
    const segmentPosition = next >= duration ? Math.max(0, duration - 0.001) : next;
    const nextSegment = Math.floor(segmentPosition / SEGMENT_SECONDS) * SEGMENT_SECONDS;
    const offset = next - nextSegment;
    positionRef.current = next;
    setPosition(next);
    pendingAbsoluteSeek.current = next;
    if (nextSegment === segmentStartRef.current) {
      pendingOffset.current = offset;
      resumeAfterLoad.current = resume;
      if (audio.current.readyState === 0) {
        setBuffering(resume);
        audio.current.load();
      } else {
        audio.current.currentTime = offset;
        if (resume) void audio.current.play().catch(() => setError("浏览器未能开始回放，请重试"));
      }
      return;
    }
    selectSegment(nextSegment, offset, resume);
  }, [index, playing, selectSegment]);

  useEffect(() => {
    if (!seekRequest || !index || lastSeekSerial.current === seekRequest.serial) return;
    const time = playbackTimeForCapture(index, seekRequest.capturedAtMs);
    if (time === null) return;
    lastSeekSerial.current = seekRequest.serial;
    seek(time, true);
  }, [seekRequest, index, seek]);

  useEffect(() => {
    if (!index || !playing) return;
    const duration = index.duration_ms / 1000;
    const nextStart = segmentStart + SEGMENT_SECONDS;
    if (nextStart >= duration || position < nextStart - PREFETCH_SECONDS || prefetched.current?.start === nextStart || prefetchingStart.current === nextStart) return;
    prefetchingStart.current = nextStart;
    void fetch(segmentUrl(sessionId, nextStart))
      .then((response) => {
        if (!response.ok) throw new Error("prefetch failed");
        return response.blob();
      })
      .then((blob) => {
        if (prefetchingStart.current !== nextStart) return;
        releasePrefetch();
        prefetched.current = { start: nextStart, url: URL.createObjectURL(blob) };
      })
      .catch(() => { if (prefetchingStart.current === nextStart) prefetchingStart.current = null; });
  }, [index, playing, position, releasePrefetch, segmentStart, sessionId]);

  if (!index) return null;
  const duration = index.duration_ms / 1000;
  const toggle = () => {
    if (!audio.current) return;
    if (audio.current.paused) {
      setBuffering(true);
      void audio.current.play().catch(() => { setBuffering(false); setError("浏览器未能开始回放，请重试"); });
    } else audio.current.pause();
  };
  const advance = () => {
    const nextStart = segmentStartRef.current + SEGMENT_SECONDS;
    if (nextStart >= duration) { setPlaying(false); positionRef.current = duration; setPosition(duration); return; }
    positionRef.current = nextStart;
    setPosition(nextStart);
    selectSegment(nextStart, 0, true);
  };
  const progress = duration > 0 ? Math.min(100, Math.max(0, position / duration * 100)) : 0;
  return <section className={`session-player ${live ? "live-player" : ""}`} aria-label="整节课程录音回放">
    <div className="session-player-label"><strong>{live ? "原音回听" : "整节课程回放"}</strong><span>{clock(position)} / {clock(duration)}</span></div>
    <audio ref={audio} preload="metadata" src={source}
      onLoadedMetadata={(event) => {
        event.currentTarget.playbackRate = speed;
        const mediaDuration = Number.isFinite(event.currentTarget.duration) ? event.currentTarget.duration : SEGMENT_SECONDS;
        event.currentTarget.currentTime = Math.min(pendingOffset.current, Math.max(0, mediaDuration - 0.01));
        pendingOffset.current = 0;
        setBuffering(false);
        if (resumeAfterLoad.current) {
          resumeAfterLoad.current = false;
          void event.currentTarget.play().catch(() => setError("浏览器未能继续回放，请点击播放"));
        }
      }}
      onCanPlay={() => setBuffering(false)}
      onWaiting={() => setBuffering(true)} onStalled={() => setBuffering(true)}
      onPlaying={() => { setPlaying(true); setBuffering(false); setError(""); }}
      onPause={() => setPlaying(false)} onEnded={advance}
      onTimeUpdate={(event) => {
        const absolute = Math.min(duration, segmentStartRef.current + event.currentTarget.currentTime);
        const target = pendingAbsoluteSeek.current;
        if (target !== null && Math.abs(absolute - target) > 0.75) return;
        pendingAbsoluteSeek.current = null;
        positionRef.current = absolute;
        setPosition(absolute);
      }}
      onError={() => { setBuffering(false); setError("录音暂时无法读取，请稍后重试"); }}
      aria-label="课程录音" />
    <div className="session-player-controls">
      <button type="button" className="player-icon-button" onClick={toggle} aria-label={playing ? "暂停回放" : "播放课程"}>{playing ? "Ⅱ" : "▶"}</button>
      <button type="button" onClick={() => seek(positionRef.current - (live ? 30 : 15))}>后退 {live ? 30 : 15} 秒</button>
      <input aria-label="回放位置" type="range" min={0} max={Math.max(duration, 0.1)} step={0.1} value={Math.min(position, duration)} style={{ "--player-progress": `${progress}%` } as CSSProperties} onChange={(event) => seek(Number(event.target.value))} />
      {!live && <button type="button" onClick={() => seek(positionRef.current + 15)}>前进 15 秒</button>}
      {!live && <label><span>速度</span><select aria-label="回放速度" value={speed} onChange={(event) => { const next = Number(event.target.value); setSpeed(next); if (audio.current) audio.current.playbackRate = next; }}>
        {[0.75, 1, 1.25, 1.5, 2].map((value) => <option key={value} value={value}>{value}×</option>)}
      </select></label>}
    </div>
    {buffering && !error && <small className="player-loading" role="status">正在加载当前录音</small>}
    {error && <small role="alert">{error}</small>}
  </section>;
}
