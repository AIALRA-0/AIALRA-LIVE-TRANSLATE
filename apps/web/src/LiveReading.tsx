import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { liveTeachingSections, visualTranslationBlocks } from "./liveReadingModel";
import type { TimelineItem } from "./types";

const UNCERTAINTY_LABEL = /不确定|待核实|听不清|存疑/;

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
  const paragraphs = useMemo(() => items.filter((item) => item.kind === "paragraph").slice(-160).reverse(), [items]);
  const latestPreview = useMemo(() => items.filter((item) => item.kind === "preview").at(-1), [items]);
  const uncertainIds = useMemo(() => paragraphs.filter((item) => {
    if (item.kind !== "paragraph") return false;
    return item.translationStale || liveTeachingSections(items, item.id).some((section) => UNCERTAINTY_LABEL.test(section.label));
  }).map((item) => item.id), [items, paragraphs]);
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
      element.scrollTo({ top: 0, behavior: "instant" });
      setNewContent(false);
    } else {
      setNewContent(true);
    }
  }, [contentKey, paragraphs.length]);

  const goLatest = () => {
    followLatest.current = true;
    setNewContent(false);
    const element = scrollRef.current;
    element?.scrollTo({ top: 0, behavior: "instant" });
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

  return <section className="live-reading" aria-label="实时课程理解" data-testid="live-reading">
    <div className="live-reading-bar">
      <div><strong>刚才这段的中文</strong><span>稳定译文优先 · 原文紧随其后</span></div>
      <div className="live-reading-actions">
        <button type="button" onClick={goUncertain} disabled={!uncertainIds.length}>待核实{uncertainIds.length ? ` · ${uncertainIds.length}` : ""}</button>
        <button type="button" onClick={goLatest}>返回最新</button>
      </div>
    </div>
    {latestPreview?.original && <div className="live-preview-strip"><strong>正在识别</strong><span>{latestPreview.original}</span></div>}
    {finished && <div className="live-review-entry"><strong>本次收音已结束</strong><span>已保存内容可继续回顾。</span><button type="button" onClick={onReview}>进入复习</button></div>}
    <div ref={scrollRef} className="live-reading-scroll" onScroll={(event) => {
      const element = event.currentTarget;
      followLatest.current = element.scrollTop < 80;
      if (followLatest.current) setNewContent(false);
    }}>
      {newContent && <button type="button" className="live-new-content" onClick={goLatest}>有新内容 · 返回最新</button>}
      {!paragraphs.length && <div className="live-empty"><strong>等待课程内容</strong><p>开始录音后，稳定中文和老师原话会出现在这里。</p></div>}
      {paragraphs.map((item) => {
        const sections = liveTeachingSections(items, item.id);
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
          {sections.length > 0 && <details className="live-teaching"><summary>展开讲解 · {sections.length} 点</summary><div>{sections.map((section, index) => <section key={`${section.label}:${index}`}><strong>{section.label}</strong><p>{section.text}</p>{section.backgroundReference && <a href={section.backgroundReference} target="_blank" rel="noopener noreferrer">查看已核对来源 ↗</a>}</section>)}</div></details>}
        </article>;
      })}
    </div>
  </section>;
}
