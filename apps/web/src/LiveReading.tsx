import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { liveTeachingSections, visualTranslationBlocks } from "./liveReadingModel";
import { ContentMarkdown } from "./ContentMarkdown";
import { safeSourceUrl } from "./contentMarkdownFormat";
import type { TimelineItem } from "./types";

const UNCERTAINTY_LABEL = /不确定|待核实|听不清|存疑/;
const READING_SETTINGS_KEY = "aialra-live-reading-v1";
type ReadingSettings = {
  chineseSize: 20 | 22 | 24;
  chineseWeight: 500 | 600 | 700;
  englishSize: 13 | 14 | 16;
  englishWeight: 400 | 500 | 600;
  density: "compact" | "standard" | "relaxed";
};
const DEFAULT_READING_SETTINGS: ReadingSettings = {
  chineseSize: 22, chineseWeight: 600, englishSize: 14, englishWeight: 400, density: "standard",
};
function readSettings(): ReadingSettings {
  try {
    const stored = JSON.parse(window.localStorage.getItem(READING_SETTINGS_KEY) ?? "null") as Partial<ReadingSettings> | null;
    if (!stored) return DEFAULT_READING_SETTINGS;
    return {
      chineseSize: [20, 22, 24].includes(stored.chineseSize ?? 0) ? stored.chineseSize as ReadingSettings["chineseSize"] : 22,
      chineseWeight: [500, 600, 700].includes(stored.chineseWeight ?? 0) ? stored.chineseWeight as ReadingSettings["chineseWeight"] : 600,
      englishSize: [13, 14, 16].includes(stored.englishSize ?? 0) ? stored.englishSize as ReadingSettings["englishSize"] : 14,
      englishWeight: [400, 500, 600].includes(stored.englishWeight ?? 0) ? stored.englishWeight as ReadingSettings["englishWeight"] : 400,
      density: ["compact", "standard", "relaxed"].includes(stored.density ?? "") ? stored.density as ReadingSettings["density"] : "standard",
    };
  } catch { return DEFAULT_READING_SETTINGS; }
}

function formatTime(value: string): string {
  return new Date(value).toLocaleTimeString("zh-CN", { hour12: false });
}

export function LiveReading({ items, sessionId, wholeAudioReady, onSeek, finished, onReview }: {
  items: TimelineItem[];
  sessionId: string;
  wholeAudioReady: boolean;
  onSeek: (capturedAtMs: number) => void;
  finished: boolean;
  onReview: () => void;
}) {
  const paragraphs = useMemo(() => items.filter((item) => item.kind === "paragraph").slice(-160), [items]);
  const latestPreview = useMemo(() => items.filter((item) => item.kind === "preview").at(-1), [items]);
  const sectionsById = useMemo(() => new Map(paragraphs.map((item) => [item.id, liveTeachingSections(items, item.id)])), [items, paragraphs]);
  const latestTeachingId = useMemo(() => paragraphs.filter((item) => (sectionsById.get(item.id)?.length ?? 0) > 0).at(-1)?.id ?? null, [paragraphs, sectionsById]);
  const [selectedTeachingId, setSelectedTeachingId] = useState<string | null>(null);
  const [selectedAtLatestId, setSelectedAtLatestId] = useState<string | null>(null);
  const [teachingPinned, setTeachingPinned] = useState(false);
  const [mobileTeachingOpen, setMobileTeachingOpen] = useState(false);
  const [settings, setSettings] = useState<ReadingSettings>(readSettings);
  useEffect(() => { window.localStorage.setItem(READING_SETTINGS_KEY, JSON.stringify(settings)); }, [settings]);
  useEffect(() => {
    const onEscape = (event: KeyboardEvent) => { if (event.key === "Escape") setMobileTeachingOpen(false); };
    window.addEventListener("keydown", onEscape);
    return () => window.removeEventListener("keydown", onEscape);
  }, []);
  const activeTeachingId = selectedTeachingId && (teachingPinned || selectedAtLatestId === latestTeachingId) && (sectionsById.get(selectedTeachingId)?.length ?? 0) > 0 ? selectedTeachingId : latestTeachingId;
  const activeSections = activeTeachingId ? sectionsById.get(activeTeachingId) ?? [] : [];
  const courseSummary = items.filter((item) => item.kind === "session-summary").at(-1);
  const hasUnderstanding = activeSections.length > 0 || Boolean(finished && courseSummary?.body.trim());
  const readingStyle = {
    "--live-zh-size": `${settings.chineseSize}px`,
    "--live-zh-weight": settings.chineseWeight,
    "--live-en-size": `${settings.englishSize}px`,
    "--live-en-weight": settings.englishWeight,
    "--live-line-height": settings.density === "compact" ? 1.4 : settings.density === "relaxed" ? 1.85 : 1.6,
  } as CSSProperties;
  const uncertainIds = useMemo(() => paragraphs.filter((item) => {
    if (item.kind !== "paragraph") return false;
    return item.translationStale || (sectionsById.get(item.id) ?? []).some((section) => UNCERTAINTY_LABEL.test(section.label));
  }).map((item) => item.id), [paragraphs, sectionsById]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const followLatest = useRef(true);
  const [newContent, setNewContent] = useState(false);
  const [paragraphAudioId, setParagraphAudioId] = useState<string | null>(null);
  const [audioError, setAudioError] = useState<string | null>(null);
  const contentKey = paragraphs.map((item) => `${item.id}:${item.translation ?? ""}:${item.translationStale ?? false}`).join("|");

  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (!element || !paragraphs.length) return;
    if (followLatest.current) {
      element.scrollTo({ top: element.scrollHeight, behavior: "instant" });
      setNewContent(false);
    } else {
      setNewContent(true);
    }
  }, [contentKey, paragraphs.length]);

  const goLatest = () => {
    followLatest.current = true;
    setNewContent(false);
    setTeachingPinned(false);
    setSelectedTeachingId(null);
    setSelectedAtLatestId(null);
    const element = scrollRef.current;
    element?.scrollTo({ top: element.scrollHeight, behavior: "instant" });
  };
  const goUncertain = () => {
    const target = uncertainIds[0];
    if (!target) return;
    followLatest.current = false;
    document.getElementById(`live-${target}`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  };
  const replay = (item: TimelineItem) => {
    setAudioError(null);
    if (wholeAudioReady && item.audioStartMs !== undefined) {
      setParagraphAudioId(null);
      onSeek(item.audioStartMs);
      return;
    }
    setParagraphAudioId((current) => current === item.id ? null : item.id);
  };

  return <section className="live-reading" style={readingStyle} aria-label="实时课程理解" data-testid="live-reading">
    <div className="live-reading-main">
      <div className="live-reading-bar">
        <div><strong>课程字幕</strong><span>按时间阅读 · 中文优先</span></div>
        <div className="live-reading-actions">
          <details className="live-reading-settings"><summary>阅读设置</summary><div className="live-reading-settings-menu">
            <label>中文字号<select aria-label="中文字号" value={settings.chineseSize} onChange={(event) => setSettings((current) => ({ ...current, chineseSize: Number(event.target.value) as ReadingSettings["chineseSize"] }))}><option value={20}>小</option><option value={22}>标准</option><option value={24}>大</option></select></label>
            <label>中文粗细<select aria-label="中文粗细" value={settings.chineseWeight} onChange={(event) => setSettings((current) => ({ ...current, chineseWeight: Number(event.target.value) as ReadingSettings["chineseWeight"] }))}><option value={500}>常规</option><option value={600}>稍粗</option><option value={700}>粗</option></select></label>
            <label>英文字号<select aria-label="英文字号" value={settings.englishSize} onChange={(event) => setSettings((current) => ({ ...current, englishSize: Number(event.target.value) as ReadingSettings["englishSize"] }))}><option value={13}>小</option><option value={14}>标准</option><option value={16}>大</option></select></label>
            <label>英文粗细<select aria-label="英文粗细" value={settings.englishWeight} onChange={(event) => setSettings((current) => ({ ...current, englishWeight: Number(event.target.value) as ReadingSettings["englishWeight"] }))}><option value={400}>常规</option><option value={500}>中等</option><option value={600}>稍粗</option></select></label>
            <label>阅读密度<select aria-label="阅读密度" value={settings.density} onChange={(event) => setSettings((current) => ({ ...current, density: event.target.value as ReadingSettings["density"] }))}><option value="compact">紧凑</option><option value="standard">标准</option><option value="relaxed">舒展</option></select></label>
          </div></details>
          <button type="button" onClick={goUncertain} disabled={!uncertainIds.length}>待核实{uncertainIds.length ? ` · ${uncertainIds.length}` : ""}</button>
          <button type="button" onClick={goLatest}>返回最新</button>
          {hasUnderstanding && <button type="button" className="live-understanding-trigger" onClick={() => setMobileTeachingOpen(true)}>当前理解</button>}
        </div>
      </div>
      {latestPreview?.original && <div className="live-preview-strip"><strong>正在识别</strong><span>{latestPreview.original}</span></div>}
      {finished && <div className="live-review-entry"><strong>本次收音已结束</strong><span>已保存内容可继续回顾。</span><button type="button" onClick={onReview}>进入复习</button></div>}
      <div ref={scrollRef} className="live-reading-scroll" onScroll={(event) => {
        const element = event.currentTarget;
        followLatest.current = element.scrollHeight - element.scrollTop - element.clientHeight < 80;
        if (followLatest.current) setNewContent(false);
      }}>
        {newContent && <button type="button" className="live-new-content" onClick={goLatest}>有新内容 · 返回最新</button>}
        {!paragraphs.length && <div className="live-empty"><strong>等待课程内容</strong><p>开始录音后，稳定中文和老师原话会出现在这里。</p></div>}
        {paragraphs.map((item) => {
          const sections = sectionsById.get(item.id) ?? [];
          const uncertain = sections.find((section) => UNCERTAINTY_LABEL.test(section.label));
          const translated = !item.translationStale && item.translationMode !== "same_language" && Boolean(item.translation?.trim());
          const blocks = translated ? visualTranslationBlocks(item.translation ?? "") : [];
          return <article id={`live-${item.id}`} key={item.id} className="live-caption" data-testid="live-caption">
            <div className="live-caption-meta"><time>{formatTime(item.occurredAt)}</time>{item.speakerLabel && <span>{item.speakerLabel}</span>}</div>
            <div className="live-translation" data-testid="live-translation">
              {translated ? blocks.map((block, index) => <p key={index}>{block}</p>) : <p className="live-translation-pending">{item.translationStale ? "原文已修订，中文译文正在更新" : item.translationMode === "same_language" ? "原文即为本节课程语言" : "中文译文生成中"}</p>}
            </div>
            <div className="live-original"><div><strong>老师原话</strong><button type="button" onClick={() => replay(item)}>{paragraphAudioId === item.id ? "收起回听" : "定位回听"}</button></div><p>{item.original}</p></div>
            {paragraphAudioId === item.id && <audio controls autoPlay preload="none" src={`/api/v1/sessions/${sessionId}/paragraphs/${item.id}/audio`} onPlay={(event) => document.querySelectorAll("audio").forEach((audio) => { if (audio !== event.currentTarget) audio.pause(); })} onError={() => setAudioError(item.id)} aria-label="当前段落录音" />}
            {audioError === item.id && <p className="live-audio-error" role="alert">这段原音暂时无法播放，请稍后重试。</p>}
            {item.translationStale && <p className="live-uncertainty" role="status">待核实：原文已修订，原译文不作为当前结果显示。</p>}
            {uncertain && !item.translationStale && <p className="live-uncertainty" role="status">{uncertain.label}：{uncertain.text}</p>}
            {sections.length > 0 && <button type="button" className="live-explanation-trigger" aria-pressed={activeTeachingId === item.id} onClick={() => { setSelectedTeachingId(item.id); setSelectedAtLatestId(latestTeachingId); setMobileTeachingOpen(true); }}>查看讲解 · {sections.length} 点</button>}
          </article>;
        })}
      </div>
    </div>
    {hasUnderstanding && mobileTeachingOpen && <button type="button" className="live-understanding-backdrop" aria-label="关闭当前理解" onClick={() => setMobileTeachingOpen(false)} />}
    {hasUnderstanding && <aside className={`live-understanding${mobileTeachingOpen ? " open" : ""}`} aria-label="当前理解">
      <div className="live-understanding-heading"><div><strong>当前理解</strong><span>{teachingPinned ? "已固定这段讲解" : "跟随最新讲解"}</span></div><div>
        {activeSections.length > 0 && <button type="button" aria-pressed={teachingPinned} onClick={() => { if (!teachingPinned) { setSelectedTeachingId(activeTeachingId); setSelectedAtLatestId(latestTeachingId); } setTeachingPinned((current) => !current); }}>{teachingPinned ? "取消固定" : "固定"}</button>}
        <button type="button" className="live-understanding-close" onClick={() => setMobileTeachingOpen(false)}>关闭</button>
      </div></div>
      {activeSections.length > 0 && <div className="live-understanding-content">{activeSections.map((section, index) => <section key={`${section.label}:${index}`}><h3>{section.label}</h3><ContentMarkdown text={section.text} />{section.backgroundReference && safeSourceUrl(section.backgroundReference) && <a href={safeSourceUrl(section.backgroundReference) ?? undefined} target="_blank" rel="noopener noreferrer">查看已核对来源 ↗</a>}</section>)}</div>}
      {finished && courseSummary?.body.trim() && <details className="live-course-summary"><summary>课程总结</summary><ContentMarkdown text={courseSummary.body} /></details>}
    </aside>}
  </section>;
}
