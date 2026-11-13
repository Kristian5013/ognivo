// The model's text as Markdown. Its own HTML is shown as text, never run or rendered:
// markdown-it escapes it (`html: false`) and refuses javascript:, vbscript:, file: and data:
// links. A link opens in the browser once the user agrees (the extension asks).

import MarkdownIt from "markdown-it";
import { memo, useMemo } from "react";

import { openLink } from "./host.ts";

const md = new MarkdownIt({ html: false, linkify: true, breaks: false, typographer: false });

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  const html = useMemo(() => md.render(text), [text]);
  return (
    <div
      className="md"
      // markdown-it's output of text with HTML escaped
      dangerouslySetInnerHTML={{ __html: html }}
      onClick={(e) => {
        const a = (e.target as HTMLElement).closest("a");
        if (!a) return;
        e.preventDefault();
        openLink(a.href);
      }}
    />
  );
});
