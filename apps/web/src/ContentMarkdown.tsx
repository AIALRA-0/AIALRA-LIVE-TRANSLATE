import { useMemo } from "react";
import { safeContentHtml } from "./contentMarkdownFormat";

export function ContentMarkdown({ text }: { text: string }) {
  const html = useMemo(() => safeContentHtml(text), [text]);
  return <div className="content-markdown" dangerouslySetInnerHTML={{ __html: html }} />;
}
