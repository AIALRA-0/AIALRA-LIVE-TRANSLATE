import { FormEvent, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { api, subscribeEvents, type DingtalkCapabilities, type RuntimeHealth } from "./api";
import { BrowserCapture } from "./audio";
import { appendEvent } from "./timeline";
import type { EventEnvelope, Session, TimelineItem } from "./types";

interface TimelineState {
  events: EventEnvelope[];
  items: TimelineItem[];
}

// Reducer actions keep replay resets separate from live append operations.
type TimelineAction =
  | { type: "append"; event: EventEnvelope }
  | { type: "reset" };

function timelineReducer(state: TimelineState, action: TimelineAction): TimelineState {
  if (action.type === "reset") return { events: [], items: [] };
  return appendEvent(state, action.event);
}

function stateLabel(state: string): string {
  return (
    {
      ready: "已就绪",
      recording: "录音中",
      degraded: "降级录音中",
      stopping: "正在停止",
      completed: "已完成",
    }[state] ?? state
  );
}

function SessionCreator({ onCreated }: { onCreated: (session: Session) => void }) {
  const [title, setTitle] = useState("今天的课程");
  const [consent, setConsent] = useState(false);
  const [demo, setDemo] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [runtime, setRuntime] = useState<RuntimeHealth | null>(null);

  useEffect(() => {
    void api.health().then(setRuntime).catch(() => setRuntime(null));
  }, []);

  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      const session = await api.createSession({
        title,
        source_language: "en",
        target_language: "zh-CN",
        consent_confirmed: consent,
        demo_mode: demo,
      });
      onCreated(session);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : "创建失败");
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="welcome-shell">
      <section className="welcome-copy">
        <div className="brand-mark" aria-hidden="true">A</div>
        <p className="eyebrow">AIALRA · LIVE LEARNING COMPANION</p>
        <h1>让课程内容<br />跟上你的理解速度。</h1>
        <p className="lede">
          直接使用电脑或手机浏览器收音，字幕、翻译和材料讲解实时进入同一条课程时间线。
        </p>
        <div className="privacy-strip">
          <span className="status-dot" /> {runtime?.processing_location ?? "正在确认处理位置"} · 第三方模型出口默认关闭
        </div>
      </section>

      <form className="session-card" onSubmit={submit}>
        <p className="section-kicker">新建课程会话</p>
        <label>
          课程名称
          <input value={title} onChange={(event) => setTitle(event.target.value)} required />
        </label>
        <div className="language-pair">
          <span>英文讲授</span><span className="arrow">→</span><span>简体中文</span>
        </div>
        <label className="check-row">
          <input
            type="checkbox"
            checked={consent}
            onChange={(event) => setConsent(event.target.checked)}
          />
          <span>我已获得课程录音许可</span>
        </label>
        <label className="check-row subtle">
          <input type="checkbox" checked={demo} onChange={(event) => setDemo(event.target.checked)} />
          <span>先用内置课程片段体验</span>
        </label>
        {error && <p className="error-message" role="alert">{error}</p>}
        <button className="primary-button" disabled={busy} type="submit">
          {busy ? "正在准备…" : "进入课程控制台"}
        </button>
        <p className="consent-note">正式录音会持续显示红色状态，并可随时停止。</p>
      </form>
    </main>
  );
}

function TimelineCard({ item }: { item: TimelineItem }) {
  return (
    <article className={`timeline-card ${item.kind}`} data-testid={`timeline-${item.kind}`}>
      <header>
        <span>{item.title}</span>
        <time>{new Date(item.occurredAt).toLocaleTimeString("zh-CN", { hour12: false })}</time>
      </header>
      {item.imageUrl && <img className="asset-preview" src={item.imageUrl} alt={item.title} />}
      <p>{item.body || "正在解析内容…"}</p>
      {item.evidenceIds.length > 0 && (
        <footer>{item.evidenceIds.map((id) => <code key={id}>{id}</code>)}</footer>
      )}
    </article>
  );
}

function Console({ initial, onExit }: { initial: Session; onExit: () => void }) {
  const [session, setSession] = useState(initial);
  const [timeline, dispatch] = useReducer(timelineReducer, { events: [], items: [] });
  const [streamConnected, setStreamConnected] = useState(false);
  const [captureStatus, setCaptureStatus] = useState("尚未连接麦克风");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  const [dingtalk, setDingtalk] = useState<DingtalkCapabilities | null>(null);
  const [dingtalkRecording, setDingtalkRecording] = useState(false);
  const [audioInputs, setAudioInputs] = useState<MediaDeviceInfo[]>([]);
  const [selectedAudioInput, setSelectedAudioInput] = useState("");
  const capture = useRef<BrowserCapture | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  // A fresh subscription replays the whole event log and then follows new events.
  useEffect(() => {
    dispatch({ type: "reset" });
    return subscribeEvents(
      initial.id,
      (event) => {
        dispatch({ type: "append", event });
        if (event.event_type === "session.stopping") {
          setSession((current) => ({ ...current, state: "stopping" }));
        }
        if (event.event_type === "session.completed") {
          setSession((current) => ({ ...current, state: "completed" }));
          setCaptureStatus("课程会话已安全结束，模型队列已排空");
        }
      },
      setStreamConnected,
    );
  }, [initial.id]);

  useEffect(() => () => capture.current?.stop(), []);

  // Capability discovery exposes credential readiness without sending a command to the A1 device.
  useEffect(() => {
    void api.dingtalkCapabilities(initial.id).then(setDingtalk).catch(() => setDingtalk(null));
  }, [initial.id]);

  useEffect(() => {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    void navigator.mediaDevices.enumerateDevices().then((devices) => {
      setAudioInputs(devices.filter((device) => device.kind === "audioinput"));
    });
  }, []);

  const counts = useMemo(
    () => ({
      segments: timeline.items.filter((item) => item.kind === "segment").length,
      pages: timeline.items.filter((item) => item.kind === "asset").length,
      cards: timeline.items.filter((item) => item.kind === "explanation").length,
    }),
    [timeline.items],
  );

  async function begin(): Promise<void> {
    setBusy(true);
    setNotice("");
    try {
      if (session.demo_mode) {
        await api.runDemo(session.id);
        setSession({ ...session, state: "recording" });
        setCaptureStatus("内置课程片段正在播放");
      } else {
        const started = await api.startSession(session.id);
        setSession(started);
        if (dingtalk?.configured) {
          try {
            await api.startDingtalk(session.id);
            setDingtalkRecording(true);
            setNotice("DingTalk A1 已同步录音；本地麦克风提供实时字幕");
          } catch (caught) {
            setNotice(
              caught instanceof Error
                ? `A1 启动失败，本地实时链路继续工作：${caught.message}`
                : "A1 启动失败，本地实时链路继续工作",
            );
          }
        }
        const nextCapture = new BrowserCapture(
          session.id,
          setCaptureStatus,
          selectedAudioInput || undefined,
        );
        capture.current = nextCapture;
        await nextCapture.start();
        const devices = await navigator.mediaDevices.enumerateDevices();
        setAudioInputs(devices.filter((device) => device.kind === "audioinput"));
      }
    } catch (caught) {
      setNotice(caught instanceof Error ? caught.message : "启动失败");
    } finally {
      setBusy(false);
    }
  }

  async function stop(): Promise<void> {
    setBusy(true);
    try {
      capture.current?.stop();
      if (dingtalkRecording) {
        try {
          await api.stopDingtalk(session.id);
          setDingtalkRecording(false);
        } catch (caught) {
          setNotice(caught instanceof Error ? caught.message : "A1 停止命令失败");
        }
      }
      const stopping = await api.stopSession(session.id);
      setSession((current) => (current.state === "completed" ? current : stopping));
      setCaptureStatus((current) =>
        current.includes("模型队列已排空")
          ? current
          : "录音已停止，正在完成已接收的字幕和译文",
      );
    } catch (caught) {
      setNotice(caught instanceof Error ? caught.message : "停止失败");
    } finally {
      setBusy(false);
    }
  }

  async function explain(): Promise<void> {
    setBusy(true);
    setNotice("正在结合最近字幕和材料生成讲解…");
    try {
      await api.explain(session.id);
      setNotice("补充讲解已加入课程时间线");
    } catch (caught) {
      setNotice(caught instanceof Error ? caught.message : "讲解失败");
    } finally {
      setBusy(false);
    }
  }

  async function upload(file: File): Promise<void> {
    setBusy(true);
    setNotice(`正在解析 ${file.name}…`);
    try {
      const result = await api.uploadAsset(session.id, file);
      setNotice(`材料解析完成，共 ${result.page_ids.length} 页`);
    } catch (caught) {
      setNotice(caught instanceof Error ? caught.message : "材料解析失败");
    } finally {
      setBusy(false);
      if (fileInput.current) fileInput.current.value = "";
    }
  }

  async function copySessionId(): Promise<void> {
    try {
      await navigator.clipboard.writeText(session.id);
      setNotice("Android 会话 ID 已复制到电脑剪贴板");
    } catch {
      setNotice(`请在 Android 客户端填写会话 ID：${session.id}`);
    }
  }

  const isRecording = ["recording", "degraded"].includes(session.state);

  return (
    <div className="console-shell">
      <header className="topbar">
        <button className="brand-button" onClick={onExit} aria-label="返回会话列表">A</button>
        <div>
          <p className="eyebrow">正在理解</p>
          <h1>{session.title}</h1>
        </div>
        <div className={`recording-pill ${isRecording ? "active" : ""}`}>
          <span /> {stateLabel(session.state)}
        </div>
      </header>

      <main className="console-grid">
        <aside className="control-rail">
          <section className="control-card hero-control">
            <p className="section-kicker">课程控制</p>
            <div className="source-badge">
              {session.demo_mode
                ? "内置体验片段"
                : dingtalk?.configured
                  ? "DingTalk A1 + 本地实时链路"
                  : "当前浏览器麦克风"}
            </div>
            {!session.demo_mode && (
              <p className={`a1-status ${dingtalk?.configured ? "ready" : ""}`}>
                {dingtalk?.configured
                  ? dingtalkRecording
                    ? "A1 正在同步录音"
                    : "A1 已配置，将随课程启动"
                  : "A1 等待企业凭据与设备验证"}
              </p>
            )}
            <p className="capture-status">{captureStatus}</p>
            {!session.demo_mode && session.state === "ready" && (
              <label className="device-picker">
                收音设备
                <select
                  value={selectedAudioInput}
                  onChange={(event) => setSelectedAudioInput(event.target.value)}
                >
                  <option value="">系统默认麦克风</option>
                  {audioInputs.map((device, index) => (
                    <option key={device.deviceId} value={device.deviceId}>
                      {device.label || `麦克风 ${index + 1}`}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {!session.demo_mode && (
              <div className="android-pairing" aria-label="Android 实机配对信息">
                <span>Android 会话 ID</span>
                <code>{session.id}</code>
                <button className="text-button" type="button" onClick={() => void copySessionId()}>
                  复制 ID
                </button>
              </div>
            )}
            {session.state === "ready" && (
              <button className="primary-button" disabled={busy} onClick={() => void begin()}>
                开始理解
              </button>
            )}
            {isRecording && (
              <button className="stop-button" disabled={busy} onClick={() => void stop()}>
                <span /> 停止并保存
              </button>
            )}
          </section>

          <section className="control-card">
            <p className="section-kicker">加入课程材料</p>
            <input
              ref={fileInput}
              className="visually-hidden"
              type="file"
              accept=".pptx,.pdf,.docx,.png,.jpg,.jpeg,.webp,.txt,.md,.csv"
              onChange={(event) => {
                const file = event.target.files?.[0];
                if (file) void upload(file);
              }}
            />
            <button className="secondary-button" disabled={busy} onClick={() => fileInput.current?.click()}>
              上传 PPT、PDF 或图片
            </button>
            <p className="micro-copy">新材料会进入下一次讲解，并保留页码引用。</p>
          </section>

          <section className="control-card stats-card">
            <div><strong>{counts.segments}</strong><span>稳定字幕</span></div>
            <div><strong>{counts.pages}</strong><span>材料页</span></div>
            <div><strong>{counts.cards}</strong><span>讲解卡</span></div>
          </section>
        </aside>

        <section className="timeline-panel">
          <div className="timeline-heading">
            <div>
              <p className="section-kicker">实时课程时间线</p>
              <h2>字幕、译文与讲解</h2>
            </div>
            <span className={`connection-label ${streamConnected ? "online" : ""}`}>
              {streamConnected ? "时间线已同步" : "正在恢复同步"}
            </span>
          </div>
          <div className="timeline-list" aria-live="polite">
            {timeline.items.length === 0 ? (
              <div className="empty-state">
                <div className="waveform" aria-hidden="true">
                  {[8, 18, 28, 14, 35, 22, 10].map((height, index) => (
                    <span key={index} style={{ height }} />
                  ))}
                </div>
                <h3>课程内容将在这里展开</h3>
                <p>开始后，稳定字幕、中文译文和材料讲解会依次进入时间线。</p>
              </div>
            ) : (
              timeline.items.map((item) => <TimelineCard key={`${item.kind}-${item.id}`} item={item} />)
            )}
          </div>
          <div className="explain-dock">
            <div>
              <strong>需要补充背景吗？</strong>
              <span>{notice || "结合最近 12 条字幕和最近 8 页材料"}</span>
            </div>
            <button className="accent-button" disabled={busy || counts.segments === 0} onClick={() => void explain()}>
              立即讲解
            </button>
          </div>
        </section>
      </main>
    </div>
  );
}

export default function App() {
  const [session, setSession] = useState<Session | null>(null);
  return session ? (
    <Console initial={session} onExit={() => setSession(null)} />
  ) : (
    <SessionCreator onCreated={setSession} />
  );
}
