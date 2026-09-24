import DOMPurify from "dompurify";
import { Marked } from "marked";

const parser = new Marked({ breaks: true, gfm: true });

// Adapted from ReadWeave's presentation-only numbered-heading normalization.
// Never rewrite facts, terms, or persisted content here.
export function normalizeContentMarkdown(source: string): string {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  let inFence = false;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (/^\s*(```|~~~)/u.test(line)) { inFence = !inFence; continue; }
    if (inFence || index >= lines.length - 2 || !line.trim() || lines[index + 1].trim()) continue;
    if (index > 0 && /^\d+(?:\.\d+)*\.[ \t]+/u.test(lines[index - 1])) continue;
    const match = line.match(/^(\d+(?:\.\d+)*\.)[ \t]+(.+)$/u);
    if (!match) continue;
    const next = lines[index + 2];
    if (!next.trim() || /^(?:[ \t]{2,}[-+*]|[ \t]*\d+[.)])[ \t]+/u.test(next)) continue;
    lines[index] = `## ${match[1]} ${match[2]}`;
  }
  return lines.join("\n");
}

export function safeContentHtml(source: string): string {
  const html = parser.parse(normalizeContentMarkdown(source));
  return DOMPurify.sanitize(html as string, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["script", "style", "iframe", "object", "form", "input", "button", "img", "audio", "video", "source", "track", "picture", "link", "meta"],
    FORBID_ATTR: ["style"],
  });
}

export function safeSourceUrl(source: string): string | null {
  try {
    const url = new URL(source);
    return url.protocol === "https:" || url.protocol === "http:" ? url.href : null;
  } catch { return null; }
}
