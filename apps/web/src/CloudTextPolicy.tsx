import { useEffect, useState } from "react";

type AiPolicy = { cloud_enabled: boolean; allowed_modalities: string[]; route_available: boolean };
type PolicyState = "loading" | "ready" | "error";

async function readPolicy(projectId: string): Promise<AiPolicy> {
  const response = await fetch(`/api/v1/projects/${encodeURIComponent(projectId)}/ai-policy`);
  if (!response.ok) throw new Error("项目授权状态读取失败，请重试");
  const policy = await response.json() as Partial<AiPolicy>;
  if (typeof policy.cloud_enabled !== "boolean" || !Array.isArray(policy.allowed_modalities) || typeof policy.route_available !== "boolean") {
    throw new Error("项目授权状态无法识别，请重试");
  }
  return { cloud_enabled: policy.cloud_enabled, allowed_modalities: policy.allowed_modalities.filter((item): item is string => typeof item === "string"), route_available: policy.route_available };
}

export function CloudTextPolicy({ projectId }: { projectId: string }) {
  const [policy, setPolicy] = useState<AiPolicy>({ cloud_enabled: false, allowed_modalities: [], route_available: false });
  const [state, setState] = useState<PolicyState>("loading");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let active = true;
    void readPolicy(projectId).then((next) => {
      if (!active) return;
      setPolicy(next);
      setState("ready");
    }).catch((caught: unknown) => {
      if (!active) return;
      setPolicy({ cloud_enabled: false, allowed_modalities: [], route_available: false });
      setError(caught instanceof Error ? caught.message : "项目授权状态读取失败，请重试");
      setState("error");
    });
    return () => { active = false; };
  }, [projectId, revision]);

  async function changeAuthorization(enabled: boolean): Promise<void> {
    const previous = policy;
    setPolicy({ ...policy, cloud_enabled: enabled, allowed_modalities: enabled ? ["text"] : [] });
    setSaving(true);
    setError("");
    try {
      const response = await fetch(`/api/v1/projects/${encodeURIComponent(projectId)}/ai-policy`, {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ cloud_enabled: enabled, allowed_modalities: enabled ? ["text"] : [] }),
      });
      if (!response.ok) throw new Error("授权未保存，请重试");
      const next = await response.json() as Partial<AiPolicy>;
      if (typeof next.cloud_enabled !== "boolean" || !Array.isArray(next.allowed_modalities) || typeof next.route_available !== "boolean") {
        throw new Error("服务器返回的授权状态无法识别，请重新读取");
      }
      const modalities = next.allowed_modalities.filter((item): item is string => typeof item === "string");
      if (next.cloud_enabled !== enabled || (enabled && (modalities.length !== 1 || modalities[0] !== "text")) || (!enabled && modalities.length !== 0)) {
        throw new Error("服务器确认的授权范围与本次选择不一致，请重新读取");
      }
      setPolicy({ cloud_enabled: next.cloud_enabled, allowed_modalities: modalities, route_available: next.route_available });
      setState("ready");
    } catch (caught) {
      setPolicy(previous);
      setError(caught instanceof Error ? caught.message : "授权未保存，请重试");
    } finally {
      setSaving(false);
    }
  }

  const enabled = state === "ready" && policy.route_available && policy.cloud_enabled && policy.allowed_modalities.length === 1 && policy.allowed_modalities[0] === "text";
  const canChange = state === "ready" && !saving && (policy.route_available || policy.cloud_enabled);
  return <section className="overview-card cloud-text-policy" aria-labelledby="cloud-text-policy-title">
    <div className="cloud-policy-heading"><div><p className="eyebrow">项目隐私</p><h2 id="cloud-text-policy-title">云端文本讲解</h2></div><span className={`policy-state${enabled ? " enabled" : ""}`} role="status">{state === "loading" ? "正在读取" : state === "error" ? "状态未知" : !policy.route_available ? "服务器未开放" : enabled ? "已授权 · 仅文本" : "默认关闭"}</span></div>
    <p>开启后，系统可将本项目的字幕和资料文本发送给已配置的云端模型，用于生成课程讲解</p>
    <p className="cloud-policy-boundary">此授权只适用于文本，不发送原始音频或图片；关闭后停止本项目的云端文本讲解</p>
    <label className="cloud-policy-toggle" htmlFor="cloud-text-policy-toggle">
      <input id="cloud-text-policy-toggle" type="checkbox" checked={policy.cloud_enabled} disabled={!canChange} onChange={(event) => void changeAuthorization(event.target.checked)} />
      <span><strong>允许本项目使用云端文本讲解</strong><small>{saving ? "正在保存项目授权…" : !policy.route_available ? "服务器未开放文本线路，不能新增授权" : enabled ? "已明确允许发送字幕和资料文本" : "未开启，不会因本开关发送文本"}</small></span>
    </label>
    {error && <div className="cloud-policy-error" role="alert"><span>{error}</span><button type="button" disabled={saving} onClick={() => { setError(""); setState("loading"); setRevision((current) => current + 1); }}>重新读取</button></div>}
  </section>;
}
