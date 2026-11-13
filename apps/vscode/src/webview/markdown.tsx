// The model's text as Markdown. Its own HTML is shown as text, never run or rendered:
// markdown-it escapes it (`html: false`) and refuses javascript:, vbscript:, file: and data:
// links. A link opens in the browser once the user agrees (the extension asks). A block of
// code has its language and a button that copies it.

import MarkdownIt from "markdown-it";
import { memo, useMemo } from "react";

import { copyText } from "./clipboard.ts";
import { highlight, known } from "./highlight.ts";
import { openLink } from "./host.ts";

const md = new MarkdownIt({ html: false, linkify: true, breaks: false, typographer: false });
// links with their scheme only: `main.rs` and `setup.py` are files, not Serbia's or
// Paraguay's sites
md.linkify.set({ fuzzyLink: false, fuzzyEmail: false });

md.renderer.rules.fence = (tokens, idx) => {
  const token = tokens[idx]!;
  const lang = token.info.trim().split(/\s+/)[0] ?? "";
  const esc = md.utils.escapeHtml;
  // highlight.js escapes what it colours
  const code = highlight(token.content.replace(/\n$/, ""), known(lang)) ?? esc(token.content.replace(/\n$/, ""));
  return `<div class="code-block"><div class="code-head"><span class="code-lang">${esc(lang)}</span><button type="button" class="code-copy" title="Copy" aria-label="Copy the code"><span class="codicon codicon-copy"></span></button></div><pre><code class="hl">${code}</code></pre></div>`;
};

export const Markdown = memo(function Markdown({ text, inline }: { text: string; inline?: boolean }) {
  const html = useMemo(() => (inline ? md.renderInline(text) : md.render(text)), [text, inline]);
  const Tag = inline ? "span" : "div";
  return (
    <Tag
      className={inline ? "md inline" : "md"}
      // markdown-it's output of text with HTML escaped
      dangerouslySetInnerHTML={{ __html: html }}
      onClick={(e) => {
        const target = e.target as HTMLElement;
        const copy = target.closest(".code-copy");
        if (copy) {
          const code = copy.closest(".code-block")?.querySelector("code")?.textContent ?? "";
          void copyText(code).then(() => {
            copy.classList.add("done");
            setTimeout(() => copy.classList.remove("done"), 1200);
          });
          return;
        }
        const a = target.closest("a");
        if (!a) return;
        e.preventDefault();
        openLink(a.href);
      }}
    />
  );
});
