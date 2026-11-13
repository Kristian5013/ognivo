// Code in a code editor's colours: highlight.js's core with the languages agents mostly
// meet, by a block's language or a file's name. Its output is the text, escaped, in spans
// of classes, which panel.css colours as VS Code's default themes do. A block is
// highlighted whole and cut into its lines after, so what spans lines (a comment, a string)
// keeps its colour on each.

import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import c from "highlight.js/lib/languages/c";
import cmake from "highlight.js/lib/languages/cmake";
import cpp from "highlight.js/lib/languages/cpp";
import csharp from "highlight.js/lib/languages/csharp";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import makefile from "highlight.js/lib/languages/makefile";
import markdown from "highlight.js/lib/languages/markdown";
import powershell from "highlight.js/lib/languages/powershell";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

const LANGUAGES = {
  bash,
  c,
  cmake,
  cpp,
  csharp,
  css,
  diff,
  dockerfile,
  go,
  ini,
  java,
  javascript,
  json,
  makefile,
  markdown,
  powershell,
  python,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};
for (const [name, language] of Object.entries(LANGUAGES)) hljs.registerLanguage(name, language);

/** A file's language by its extension. */
const BY_EXTENSION: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  pyi: "python",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hh: "cpp",
  hxx: "cpp",
  cs: "csharp",
  go: "go",
  java: "java",
  json: "json",
  jsonc: "json",
  jsonl: "json",
  toml: "ini",
  ini: "ini",
  cfg: "ini",
  conf: "ini",
  yaml: "yaml",
  yml: "yaml",
  md: "markdown",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  ps1: "powershell",
  psm1: "powershell",
  sql: "sql",
  html: "xml",
  htm: "xml",
  xml: "xml",
  svg: "xml",
  vue: "xml",
  css: "css",
  scss: "css",
  less: "css",
  mk: "makefile",
  cmake: "cmake",
  diff: "diff",
  patch: "diff",
};

/** Files whose name says their language. */
const BY_NAME: Record<string, string> = {
  makefile: "makefile",
  gnumakefile: "makefile",
  dockerfile: "dockerfile",
  "cmakelists.txt": "cmake",
};

/** The language of a file, by its name; none for one highlight.js here does not know. */
export function languageOf(path: string): string | undefined {
  const name = path.replace(/\\/g, "/").split("/").pop()!.toLowerCase();
  if (BY_NAME[name]) return BY_NAME[name];
  const dot = name.lastIndexOf(".");
  return dot < 0 ? undefined : BY_EXTENSION[name.slice(dot + 1)];
}

/** A block's language as Markdown names it (`ts`, `rust`, `c++`, `sh`), if known here. */
export function known(language: string): string | undefined {
  const lang = language.toLowerCase();
  if (!lang) return undefined;
  return hljs.getLanguage(lang) ? lang : BY_EXTENSION[lang];
}

/** Code as HTML; `undefined` in a language not known here. */
export function highlight(code: string, language: string | undefined): string | undefined {
  if (!language || !hljs.getLanguage(language)) return undefined;
  try {
    return hljs.highlight(code, { language, ignoreIllegals: true }).value;
  } catch {
    return undefined;
  }
}

/** Highlighted HTML cut at its line ends: each line closes the spans open at its end, and
 * the next opens them again. highlight.js writes nothing but spans and escaped text. */
export function splitLines(html: string): string[] {
  const out: string[] = [];
  const open: string[] = [];
  let line = "";
  let i = 0;
  while (i < html.length) {
    const ch = html[i]!;
    if (ch === "<") {
      const end = html.indexOf(">", i);
      const tag = html.slice(i, end + 1);
      if (tag.startsWith("</")) open.pop();
      else open.push(tag);
      line += tag;
      i = end + 1;
    } else if (ch === "\n") {
      out.push(line + "</span>".repeat(open.length));
      line = open.join("");
      i++;
    } else {
      line += ch;
      i++;
    }
  }
  out.push(line);
  return out;
}

/** Each of `lines` as HTML, highlighted together; `undefined` in a language not known
 * here, or for more lines than are worth it. */
export function highlightLines(lines: string[], language: string | undefined): string[] | undefined {
  if (!language || lines.length > 2000) return undefined;
  const html = highlight(lines.join("\n"), language);
  if (html === undefined) return undefined;
  const split = splitLines(html);
  return split.length === lines.length ? split : undefined;
}
