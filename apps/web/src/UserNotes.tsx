import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./api";

export function UserNotes({ sessionId }: { sessionId: string }) {
  const [text, setText] = useState("");
  const [ready, setReady] = useState(false);
  const [status, setStatus] = useState("正在读取笔记…");
  const [blocked, setBlocked] = useState(false);
  const [version, setVersion] = useState(0);
  const revision = useRef(0);
  const saved = useRef("");
  const latest = useRef("");
  const saving = useRef(false);
  const key = `aialra-note-draft:${sessionId}`;

  useEffect(() => {
    let active = true;
    void api.note(sessionId).then((note) => {
      if (!active) return;
      revision.current = note.revision;
      saved.current = note.text;
      let draft: {text: string; revision: number} | null = null;
      try { draft = JSON.parse(sessionStorage.getItem(key) ?? "null"); } catch { /* preserve remote */ }
      const hasDraft = draft && typeof draft.text === "string" && draft.text !== note.text;
      latest.current = hasDraft && draft ? draft.text : note.text;
      setText(latest.current);
      const conflict = Boolean(hasDraft && draft && draft.revision !== note.revision);
      setBlocked(conflict);
      setStatus(conflict ? "另一页面已更新。下方是本页草稿，请先复制保留，再读取最新版本。" : hasDraft ? "已恢复本页草稿，准备保存" : "已保存 · 仅由你编辑，不会被模型覆盖");
      setReady(true);
    }).catch(() => { if (active) { setBlocked(true); setStatus("笔记读取失败，请检查网络后重新进入；已有草稿仍保留。"); } });
    return () => { active = false; };
  }, [sessionId, key]);

  const save = useCallback(async () => {
    if (!ready || saving.current || blocked || latest.current === saved.current) return;
    saving.current = true;
    const value = latest.current;
    setStatus("正在保存…");
    try {
      const result = await api.saveNote(sessionId, value, revision.current);
      revision.current = result.revision;
      saved.current = value;
      if (latest.current === value) sessionStorage.removeItem(key);
      else sessionStorage.setItem(key, JSON.stringify({text: latest.current, revision: revision.current}));
      setStatus(latest.current === value ? "已保存 · 历史版本保留" : "有新修改，等待保存");
    } catch (error) {
      setBlocked(true);
      setStatus(error instanceof Error ? error.message : "保存失败，本页草稿已保留");
    } finally { saving.current = false; setVersion((current) => current + 1); }
  }, [ready, blocked, sessionId, key]);

  useEffect(() => {
    const timer = setTimeout(() => void save(), 800);
    return () => clearTimeout(timer);
  }, [text, version, save]);

  useEffect(() => {
    const warn = (event: BeforeUnloadEvent) => {
      if (latest.current !== saved.current) { event.preventDefault(); event.returnValue = ""; }
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, []);

  return <section className="user-notes-editor" aria-label="我的笔记">
    <h2>我的笔记</h2><p role="status">{status}</p>
    <textarea aria-label="课程笔记" disabled={!ready} value={text} placeholder="记录你的想法、疑问和补充。输入后自动保存。" onChange={(event) => {
      latest.current = event.target.value; setText(latest.current);
      sessionStorage.setItem(key, JSON.stringify({text: latest.current, revision: revision.current}));
    }} />
    {blocked && ready && <div><button type="button" onClick={() => { setBlocked(false); setVersion((v) => v + 1); }}>重试保存（不会覆盖其他页面的新版本）</button><button type="button" onClick={() => {
      if (!window.confirm("请先复制下方草稿。读取最新版本会替换本页编辑框，是否继续？")) return;
      void api.note(sessionId).then((note) => { revision.current = note.revision; saved.current = note.text; latest.current = note.text; setText(note.text); sessionStorage.removeItem(key); setBlocked(false); setStatus("已读取最新版本"); }).catch(() => setStatus("读取失败，草稿未改变"));
    }}>读取最新版本</button></div>}
  </section>;
}
