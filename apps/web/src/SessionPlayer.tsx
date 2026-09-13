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
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    api.sessionAudioIndex(sessionId).then((next) => {
      if (active) { setIndex(next); setError(""); onReady(true); }
    }).catch(() => { if (active) { setIndex(null); onReady(false); } });
    return () => { active = false; };
  }, [sessionId, sessionState, onReady]);

  useEffect(() => {
    if (!seekRequest || !index || !audio.current || lastSeekSerial.current === seekRequest.serial) return;
    const time = playbackTimeForCapture(index, seekRequest.capturedAtMs);
    if (time === null) return;
    lastSeekSerial.current = seekRequest.serial;
    audio.current.currentTime = time;
    void audio.current.play().catch(() => setError("浏览器未能开始回放，请点击播放器的播放键"));
  }, [seekRequest, index]);

  if (!index) return null;
  return <section className="session-player" aria-label="整节课程录音回放">
    <div className="session-player-label"><strong>整节课程回放</strong><span>{clock(position)} / {clock(index.duration_ms / 1000)}</span></div>
    <audio ref={audio} controls preload="metadata" src={`/api/v1/sessions/${sessionId}/audio`}
      onTimeUpdate={(event) => setPosition(event.currentTarget.currentTime)}
      onError={() => setError("录音暂时无法读取，请稍后重试")}
      aria-label="课程录音" />
    <div className="session-player-actions">
      <button type="button" onClick={() => { if (audio.current) audio.current.currentTime = Math.max(0, audio.current.currentTime - 15); }}>后退 15 秒</button>
      <button type="button" onClick={() => { if (audio.current) audio.current.currentTime = Math.min(index.duration_ms / 1000, audio.current.currentTime + 15); }}>前进 15 秒</button>
      <label>速度 <select value={speed} onChange={(event) => { const next = Number(event.target.value); setSpeed(next); if (audio.current) audio.current.playbackRate = next; }}>
        {[0.75, 1, 1.25, 1.5, 2].map((value) => <option key={value} value={value}>{value}×</option>)}
      </select></label>
    </div>
    {error && <small role="status">{error}</small>}
  </section>;
}
