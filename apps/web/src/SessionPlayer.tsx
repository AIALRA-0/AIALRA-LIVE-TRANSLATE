import { useEffect, useRef, useState } from "react";
import { api, type SessionAudioIndex } from "./api";
import { playbackTimeForCapture } from "./sessionPlayback";

function clock(seconds: number): string {
  if (!Number.isFinite(seconds)) return "00:00";
  const whole = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor(whole / 60) % 60;
  const rest = whole % 60;
  return hours ? `${hours}:${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`
    : `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}

export function SessionPlayer({ sessionId, sessionState, seekRequest, onReady }: {
  sessionId: string;
  sessionState: string;
  seekRequest: { capturedAtMs: number; serial: number } | null;
  onReady: (ready: boolean) => void;
}) {
  const audio = useRef<HTMLAudioElement>(null);
  const lastSeekSerial = useRef(0);
  const [index, setIndex] = useState<SessionAudioIndex | null>(null);
  const [position, setPosition] = useState(0);
  const [speed, setSpeed] = useState(1);
  const [playing, setPlaying] = useState(false);
  const [buffering, setBuffering] = useState(false);
  const [mediaReady, setMediaReady] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    api.sessionAudioIndex(sessionId).then((next) => {
      if (active) { setIndex(next); setError(""); }
    }).catch(() => { if (active) { setIndex(null); onReady(false); } });
    return () => { active = false; };
  }, [sessionId, sessionState, onReady]);

  useEffect(() => { onReady(Boolean(index && mediaReady)); }, [index, mediaReady, onReady]);

  useEffect(() => {
    if (!seekRequest || !index || !audio.current || lastSeekSerial.current === seekRequest.serial) return;
    const time = playbackTimeForCapture(index, seekRequest.capturedAtMs);
    if (time === null) return;
    lastSeekSerial.current = seekRequest.serial;
    audio.current.currentTime = time;
    void audio.current.play().catch(() => setError("浏览器未能开始回放，请点击播放"));
  }, [seekRequest, index]);

  if (!index) return null;
  const duration = index.duration_ms / 1000;
  const seek = (next: number) => {
    if (!audio.current) return;
    audio.current.currentTime = Math.max(0, Math.min(duration, next));
    setPosition(audio.current.currentTime);
  };
  const toggle = () => {
    if (!audio.current) return;
    if (audio.current.paused) void audio.current.play().catch(() => setError("浏览器未能开始回放，请重试"));
    else audio.current.pause();
  };
  return <section className="session-player" aria-label="整节课程录音回放">
    <div className="session-player-label"><strong>整节课程回放</strong><span>{clock(position)} / {clock(duration)}</span></div>
    <audio ref={audio} preload="none" src={`/api/v1/sessions/${sessionId}/audio`}
      onLoadedMetadata={() => { setMediaReady(true); setBuffering(false); }}
      onCanPlay={() => { setMediaReady(true); setBuffering(false); }}
      onWaiting={() => setBuffering(true)} onStalled={() => setBuffering(true)}
      onPlaying={() => { setPlaying(true); setBuffering(false); setError(""); }}
      onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)}
      onTimeUpdate={(event) => setPosition(event.currentTarget.currentTime)}
      onError={() => { setMediaReady(false); setBuffering(false); setError("录音暂时无法读取，请稍后重试"); }}
      aria-label="课程录音" />
    <div className="session-player-controls">
      <button type="button" className="player-icon-button" onClick={toggle} aria-label={playing ? "暂停回放" : "播放课程"}>{playing ? "Ⅱ" : "▶"}</button>
      <button type="button" onClick={() => seek(position - 15)}>后退 15 秒</button>
      <input aria-label="回放位置" type="range" min={0} max={Math.max(duration, 0.1)} step={0.1} value={Math.min(position, duration)} onChange={(event) => seek(Number(event.target.value))} />
      <button type="button" onClick={() => seek(position + 15)}>前进 15 秒</button>
      <label><span>速度</span><select aria-label="回放速度" value={speed} onChange={(event) => { const next = Number(event.target.value); setSpeed(next); if (audio.current) audio.current.playbackRate = next; }}>
        {[0.75, 1, 1.25, 1.5, 2].map((value) => <option key={value} value={value}>{value}×</option>)}
      </select></label>
    </div>
    {buffering && !error && <small className="player-loading" role="status">正在加载下一段录音</small>}
    {error && <small role="alert">{error}</small>}
  </section>;
}
