// A call in the thread, the way it reads best:
// - what reads and searches is a line: what it looked at and what it found, which opens to
//   its result (thread.tsx folds such lines of a turn into one exploration);
// - a command is a line too: the command and how it ended, which opens to its output (open
//   when it failed), and the terminal that shows the agent's commands;
// - a change is a file's card: its name and the lines it added and took out; a click opens
//   the file, another the change in the editor's diff, and the card opens to its lines.
// A call the model still writes shows at the conversation's end, its last lines as they
// come (DraftPreview).

import { type ReactNode, memo, useLayoutEffect, useRef, useState } from "react";

import { highlightLines, languageOf } from "./highlight.ts";
import { openDiff, openFile, showCommands } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { type FileDiff, diffsOf, splitPath } from "./model/diff.ts";
import { body, draftText, header, plural, refusalReason, seconds } from "./model/format.ts";
import type { Call, ToolItem } from "./model/model.ts";
import type { Draft } from "./model/store.ts";

export type CallState = "running" | "ok" | "failed" | "refused";

export function callState(item: ToolItem): CallState {
  const r = item.result;
  if (!r) return "running";
  if (r.outcome === "refused") return "refused";
  if (r.outcome === "error") return "failed";
  if (r.exit !== undefined && r.exit !== 0) return "failed";
  if (/^\[shell: (timed out|killed)/.test(r.text)) return "failed";
  return "ok";
}

/** How a call shows: a command's card, a change's diff, or a line. */
export function shape(tool: string | undefined): "command" | "change" | "line" {
  if (tool === "Bash") return "command";
  if (tool === "Edit" || tool === "Write" || tool === "ApplyPatch") return "change";
  return "line";
}

/** What the tool's own first line says it found, in few words. */
function found(item: ToolItem): string | undefined {
  const h = header(item);
  if (!h) return undefined;
  switch (item.call?.name) {
    case "Read": {
      const range = /lines (\d+)-(\d+) of (\d+) lines/.exec(h);
      if (range) {
        const [, from, to, all] = range;
        return from === "1" && to === all ? plural(Number(all), "line") : `lines ${from}–${to} of ${all}`;
      }
      const whole = /(\d+) of (\d+) lines/.exec(h);
      return whole ? plural(Number(whole[2]), "line") : undefined;
    }
    case "Grep": {
      const m = /^(\d+) match(?:es)? in (\d+) files?/.exec(h);
      if (!m) return /^no match/.test(h) ? "no matches" : undefined;
      return m[1] === "0" ? "no matches" : `${plural(Number(m[1]), "match", "matches")} in ${plural(Number(m[2]), "file")}`;
    }
    case "Glob": {
      const m = /^(\d+) files? match/.exec(h);
      return m ? (m[1] === "0" ? "no files" : plural(Number(m[1]), "file")) : undefined;
    }
    default:
      return undefined;
  }
}

function FileName({ path }: { path: string }) {
  const { name, dir } = splitPath(path);
  return (
    <>
      <span className="fname">{name}</span>
      {dir && <span className="fdir">{dir}</span>}
    </>
  );
}

/** A line's icon, what the call did, and what it did it to. */
function lineOf(item: ToolItem): { icon: string; verb: string; target: ReactNode; file?: { path: string; line?: number } } {
  const c = item.call;
  switch (c?.name) {
    case "Read":
      return {
        icon: "file",
        verb: "Read",
        target: <FileName path={c.path} />,
        file: { path: c.path, line: c.offset && c.offset > 1 ? c.offset : undefined },
      };
    case "Grep":
      return {
        icon: "search",
        verb: "Searched",
        target: (
          <>
            <code>{c.pattern}</code>
            {c.path && <span className="fdir">in {c.path}</span>}
            {c.glob && <span className="fdir">{c.glob}</span>}
          </>
        ),
      };
    case "Glob":
      return {
        icon: "list-tree",
        verb: "Listed",
        target: (
          <>
            <code>{c.pattern}</code>
            {c.path && <span className="fdir">in {c.path}</span>}
          </>
        ),
      };
    case "ToolSearch":
      return { icon: "search-fuzzy", verb: "Looked for tools", target: <span>«{c.query}»</span> };
    case "Mcp":
      return {
        icon: "plug",
        verb: "Called",
        target: (
          <>
            <span className="fname">{c.tool}</span>
            <span className="fdir">{c.server}</span>
          </>
        ),
      };
    case "Skill":
      return { icon: "book", verb: "Used the skill", target: <span className="fname">{c.skill}</span> };
    case "Task":
      return { icon: "organization", verb: "Delegated", target: <span>{plural(c.tasks.length, "task")}</span> };
    default:
      return { icon: "tools", verb: item.shown, target: <code>{typeof item.raw === "string" ? item.raw : JSON.stringify(item.raw)}</code> };
  }
}

/** Lines of a result the panel draws at most; the rest is in the editor. */
const MAX_LINES = 500;

function Output({ text, className }: { text: string; className?: string }) {
  if (!text.trim()) return <p className="out-note">No output</p>;
  const lines = text.split("\n");
  const cut = lines.length > MAX_LINES ? `${lines.slice(0, MAX_LINES).join("\n")}\n… ${lines.length - MAX_LINES} more lines` : text;
  return <pre className={`out${className ? ` ${className}` : ""}`}>{cut}</pre>;
}

/** A file's lines as the read tool gives them, numbered: the numbers in a gutter. */
const NUMBERED = /^\s*(\d+)(?:\t|│)(.*)$/;

function CodeView({ text, path }: { text: string; path: string }) {
  const lines = text.replace(/\n$/, "").split("\n").slice(0, MAX_LINES);
  const parsed = lines.map((l) => NUMBERED.exec(l));
  if (!parsed.some(Boolean)) return <Output text={text} />;
  // the file's lines highlighted together, ognivo's notes among them left out
  const code = parsed.flatMap((m) => (m ? [m[2]!] : []));
  const html = highlightLines(code, languageOf(path));
  let n = 0;
  return (
    <div className="out code hl">
      <div className="out-lines">
        {lines.map((l, i) => {
          const m = parsed[i];
          if (!m) {
            return (
              <div key={i} className="cl note">
                <span className="ln" />
                <span className="tx">{l}</span>
              </div>
            );
          }
          const lit = html?.[n++];
          return (
            <div key={i} className="cl">
              <span className="ln">{m[1]}</span>
              {lit ? <span className="tx" dangerouslySetInnerHTML={{ __html: lit }} /> : <span className="tx">{m[2] || " "}</span>}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** grep's lines: where each match is, then the line. */
const MATCH = /^(.+?):(\d+):(.*)$/;

function MatchesView({ text }: { text: string }) {
  const lines = text.replace(/\n$/, "").split("\n").slice(0, MAX_LINES);
  return (
    <div className="out code">
      <div className="out-lines">
        {lines.map((l, i) => {
          const m = MATCH.exec(l);
          return m ? (
            <div key={i} className="cl">
              <span className="gp">
                {m[1]}:{m[2]}
              </span>
              <span className="tx">{m[3] || " "}</span>
            </div>
          ) : (
            <div key={i} className="cl note">
              <span className="tx">{l}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function LineBody({ item }: { item: ToolItem }) {
  const r = item.result;
  if (!r) return null;
  if (r.outcome === "refused") return <p className="out-note">Not allowed: {refusalReason(r.text)}</p>;
  if (r.outcome === "error") return <Output text={r.text} />;
  if (r.images?.length) {
    return (
      <div className="pictures">
        {r.images.map((p, i) => (
          <img key={i} src={`data:${p.mime};base64,${p.data}`} alt={`image ${i + 1} the call read`} />
        ))}
      </div>
    );
  }
  const call = item.call;
  if (call?.name === "Read") return <CodeView text={body(item)} path={call.path} />;
  if (call?.name === "Grep") return <MatchesView text={body(item)} />;
  if (call?.name === "Mcp") return <Output text={`${call.arguments}\n\n${body(item)}`} />;
  if (!call) return <Output text={JSON.stringify(item.raw, null, 2)} />;
  return <Output text={body(item)} />;
}

export function ToolLine({ item, open, onOpen, project }: { item: ToolItem; open?: boolean; onOpen: (open: boolean) => void; project: string }) {
  const state = callState(item);
  const { icon, verb, target, file } = lineOf(item);
  const meta = state === "ok" ? found(item) : undefined;
  return (
    <div className={`tline is-${state}`}>
      <div className="tline-head">
        <button type="button" className="tline-toggle" aria-expanded={Boolean(open)} disabled={state === "running"} onClick={() => onOpen(!open)}>
          <Icon name={state === "running" ? "loading" : icon} spin={state === "running"} className="tline-icon" />
          <span className="tline-verb">{verb}</span>
          <span className="tline-target">{target}</span>
          {meta && <span className="tline-meta">{meta}</span>}
          {state === "failed" && <span className="tag err">failed</span>}
          {state === "refused" && <span className="tag warn">not allowed</span>}
          {state !== "running" && <Icon name="chevron-right" className="chev" />}
        </button>
        {file && <IconButton icon="go-to-file" title={`Open ${file.path}`} className="reveal" onClick={() => openFile(file.path, project, file.line)} />}
      </div>
      {open && state !== "running" && (
        <div className="tline-body">
          <LineBody item={item} />
        </div>
      )}
    </div>
  );
}

/** How a command ended, after it: its exit code where that is not 0, and how long it took. */
function CommandFacts({ item, state }: { item: ToolItem; state: CallState }) {
  const r = item.result;
  if (!r || state === "refused") return null;
  const killed = /^\[shell: (timed out|killed)/.exec(r.text);
  // a command of a moment says no time
  const time = r.seconds !== undefined && r.seconds >= 0.1 ? seconds(r.seconds * 1000) : undefined;
  const ended = killed ? (killed[1] === "timed out" ? "timed out" : "killed") : r.exit !== undefined && r.exit !== 0 ? `exit ${r.exit}` : state === "failed" ? "failed" : undefined;
  if (!ended && !time) return null;
  return (
    <span className="cmdrow-facts">
      {ended && <span className="err">{ended}</span>}
      {ended && time && " · "}
      {time}
    </span>
  );
}

const COMMAND_ICON: Record<CallState, string> = { running: "loading", ok: "check", failed: "error", refused: "circle-slash" };
const COMMAND_VERB: Record<CallState, string> = { running: "Running", ok: "Ran", failed: "Ran", refused: "Not run" };

/** A command: a line of what ran and how it ended, open to its output when it failed. */
function CommandRow({ item, open, onOpen }: { item: ToolItem; open?: boolean; onOpen: (open: boolean) => void }) {
  const call = item.call as Extract<Call, { name: "Bash" }>;
  const state = callState(item);
  const expanded = state !== "running" && (open ?? state === "failed");
  const [first, ...more] = call.command.split("\n");
  return (
    <div className={`cmdrow is-${state}`}>
      <div className="cmdrow-head">
        <button
          type="button"
          className="cmdrow-toggle"
          aria-expanded={expanded}
          disabled={state === "running"}
          title={call.command}
          onClick={() => onOpen(!expanded)}
        >
          <Icon name={COMMAND_ICON[state]} spin={state === "running"} className="cmdrow-icon" />
          <span className="cmdrow-verb">{COMMAND_VERB[state]}</span>
          <code className="cmdrow-line">{first}</code>
          {more.length > 0 && <span className="cmdrow-more">+{plural(more.length, "line")}</span>}
          <CommandFacts item={item} state={state} />
          {state !== "running" && <Icon name="chevron-right" className="chev" />}
        </button>
        <button
          type="button"
          className="cmdrow-terminal reveal"
          title="Show the agent's commands in a terminal"
          onClick={() => void showCommands().catch(() => undefined)}
        >
          <Icon name="terminal" />
          <span>Terminal</span>
        </button>
      </div>
      {expanded && (
        <div className="cmdrow-body">
          {more.length > 0 && <pre className="out cmd-full">{call.command}</pre>}
          {state === "refused" ? (
            <p className="out-note">Not allowed: {refusalReason(item.result!.text)}</p>
          ) : (
            <Output text={body(item)} className="term" />
          )}
        </div>
      )}
    </div>
  );
}

export function Delta({ added, removed }: { added: number; removed: number }) {
  return (
    <span className="delta">
      {added > 0 && <span className="plus">+{added}</span>}
      {removed > 0 && <span className="minus">−{removed}</span>}
    </span>
  );
}

/** Lines of a diff the card shows when opened, before a click shows all. */
const PREVIEW = 60;

/** A change's lines: what it took out and put in, highlighted in the file's language. */
function DiffLines({ diff }: { diff: FileDiff }) {
  const [all, setAll] = useState(false);
  const shown = all ? diff.lines : diff.lines.slice(0, PREVIEW);
  const hidden = diff.lines.length - shown.length;
  // the lines highlighted together, what was and what is
  const html = highlightLines(
    shown.map((l) => (l.kind === "gap" ? "" : l.text)),
    languageOf(diff.path),
  );
  return (
    <div className="edit-body">
      <div className="fdiff-body">
        <div className="fdiff-lines">
          {shown.map((l, i) =>
            l.kind === "gap" ? (
              <div key={i} className="dl gap">
                ⋯
              </div>
            ) : (
              <div key={i} className={`dl ${l.kind}`}>
                <span className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</span>
                {html?.[i] ? <span className="tx" dangerouslySetInnerHTML={{ __html: html[i] }} /> : <span className="tx">{l.text || " "}</span>}
              </div>
            ),
          )}
        </div>
      </div>
      {hidden > 0 && (
        <button type="button" className="fdiff-more" onClick={() => setAll(true)}>
          Show {plural(hidden, "more line", "more lines")}
        </button>
      )}
    </div>
  );
}

/** A file a call changed: its name and lines, which open the file, the change in the
 * editor's diff, and its lines here. */
function FileEdit({
  diff,
  state,
  open,
  onOpen,
  project,
  thread,
}: {
  diff: FileDiff;
  state: CallState;
  open: boolean;
  onOpen: (open: boolean) => void;
  project: string;
  thread?: number;
}) {
  const { name, dir } = splitPath(diff.path);
  const lines = diff.lines.length > 0;
  return (
    <div className={`edit is-${state}${open && lines ? " open" : ""}`}>
      <div className="edit-head">
        <button type="button" className="edit-open" title={`Open ${diff.path}`} onClick={() => openFile(diff.path, project, diff.at)}>
          <Icon name={diff.created ? "new-file" : diff.deleted ? "trash" : "file"} className="edit-icon" />
          <span className="edit-name">{name}</span>
          {dir && <span className="fdir">{dir}</span>}
          <Delta added={diff.added} removed={diff.removed} />
        </button>
        {diff.created && <span className="tag">new</span>}
        {diff.deleted && <span className="tag">deleted</span>}
        {state === "running" && <Icon name="loading" spin className="edit-state" />}
        {state === "failed" && <span className="tag err">failed</span>}
        {state === "refused" && <span className="tag warn">not allowed</span>}
        {state === "ok" && thread !== undefined && (
          <IconButton icon="diff" title={`Show the change to ${name} in a diff`} className="edit-act" onClick={() => openDiff(thread, diff.path, project)} />
        )}
        {lines && (
          <IconButton
            icon="chevron-down"
            title={open ? `Hide the lines of ${name}` : `Show the lines of ${name}`}
            className={`edit-act edit-lines${open ? " on" : ""}`}
            onClick={() => onOpen(!open)}
          />
        )}
      </div>
      {open && lines && <DiffLines diff={diff} />}
    </div>
  );
}

function firstLines(text: string, n = 3): string {
  return text.replace(/^\[(error|refused)\]\s*/, "").split("\n").slice(0, n).join("\n");
}

/** A change: a card for each file it changed, and why it did not apply. */
function EditCard({
  item,
  open,
  onOpen,
  project,
  thread,
}: {
  item: ToolItem;
  open?: boolean;
  onOpen: (open: boolean) => void;
  project: string;
  thread?: number;
}) {
  const state = callState(item);
  const files = diffsOf(item);
  const r = item.result;
  return (
    <div className={`edits is-${state}`}>
      {files.map((f) => (
        <FileEdit key={f.path} diff={f} state={state} open={Boolean(open)} onOpen={onOpen} project={project} thread={thread} />
      ))}
      {files.length === 0 && <p className="out-note">{item.shown}: no file in its arguments</p>}
      {r && (state === "failed" || state === "refused") && (
        <p className="change-why">{state === "refused" ? `Not allowed: ${refusalReason(r.text)}` : firstLines(r.text)}</p>
      )}
    </div>
  );
}

export const ToolView = memo(function ToolView({
  item,
  open,
  onOpen,
  project,
  thread,
}: {
  item: ToolItem;
  open?: boolean;
  onOpen: (open: boolean) => void;
  project: string;
  thread?: number;
}) {
  switch (shape(item.call?.name)) {
    case "command":
      return <CommandRow item={item} open={open} onOpen={onOpen} />;
    case "change":
      return <EditCard item={item} open={open} onOpen={onOpen} project={project} thread={thread} />;
    default:
      return <ToolLine item={item} open={open} onOpen={onOpen} project={project} />;
  }
});

/** Lines of a call being written that show at the conversation's end. */
const WRITTEN_LINES = 5;

/** The last lines of a call the model writes: a file's text, a patch, a command, in its
 * language, the top fading out. */
export const DraftPreview = memo(function DraftPreview({ draft }: { draft: Draft }) {
  const shown = draftText(draft);
  // the lines' box cuts what goes above it: lines before the last ones, or long lines that
  // wrap; its top fades out where it cuts
  const box = useRef<HTMLDivElement>(null);
  const [over, setOver] = useState(false);
  useLayoutEffect(() => {
    // the lines sit at the bottom, so what is cut is above the box's top: no scroll height
    // says it, the first line's place does
    const el = box.current;
    const first = el?.firstElementChild;
    if (el && first) setOver(first.getBoundingClientRect().top < el.getBoundingClientRect().top - 1);
  });
  if (!shown) return null;
  const all = shown.text.replace(/\r\n?/g, "\n").split("\n");
  const from = Math.max(0, all.length - WRITTEN_LINES);
  const lines = all.slice(from);
  const language = shown.kind === "command" ? "bash" : shown.kind === "patch" ? "diff" : shown.path ? languageOf(shown.path) : undefined;
  const html = highlightLines(lines, language);
  return (
    <div className="live-preview" aria-hidden="true">
      <div ref={box} className={`live-preview-lines${from > 0 || over ? " cut" : ""}`}>
        {lines.map((l, i) => (
          <div key={from + i} className="lp">
            {html?.[i] ? <span dangerouslySetInnerHTML={{ __html: html[i] }} /> : <span>{l || " "}</span>}
            {i === lines.length - 1 && <span className="caret" />}
          </div>
        ))}
      </div>
    </div>
  );
});
