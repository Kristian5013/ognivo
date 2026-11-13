// A call in the thread, the way it reads best:
// - what reads and searches is a line: what it looked at and what it found, which opens to
//   its result;
// - a command is a terminal's card: the command, how it ended, its output (open when it
//   failed);
// - a change is the file's diff: what it added and took out, its first lines, all of them
//   on a click.
// A call the model still writes shows as it comes. A file opens in the editor, a change in
// the editor's diff.

import { type ReactNode, memo } from "react";

import { highlightLines, languageOf } from "./highlight.ts";
import { openDiff, openFile } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { type FileDiff, diffsOf, splitPath } from "./model/diff.ts";
import { body, header, refusalReason, seconds } from "./model/format.ts";
import type { Call, ToolItem } from "./model/model.ts";
import type { Draft, DraftState } from "./model/store.ts";

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

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
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

function ToolLine({ item, open, onOpen, project }: { item: ToolItem; open?: boolean; onOpen: (open: boolean) => void; project: string }) {
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

/** How a command ended, on the right of its card. */
function CommandStatus({ item, state }: { item: ToolItem; state: CallState }) {
  const r = item.result;
  const killed = r ? /^\[shell: (timed out|killed)/.exec(r.text) : null;
  // a command of a moment says no time
  const time = r?.seconds !== undefined && r.seconds >= 0.1 && state !== "refused" ? <span className="muted">{seconds(r.seconds * 1000)}</span> : null;
  if (state === "running") {
    return (
      <span className="cmd-status">
        <Icon name="loading" spin /> <span className="muted">Running</span>
      </span>
    );
  }
  if (state === "refused") return <span className="cmd-status warn">Not run</span>;
  return (
    <span className="cmd-status">
      {killed ? (
        <span className="err">{killed[1] === "timed out" ? "Timed out" : "Killed"}</span>
      ) : r?.exit !== undefined && r.exit !== 0 ? (
        <span className="err">Exit {r.exit}</span>
      ) : state === "failed" ? (
        <span className="err">Failed</span>
      ) : (
        <Icon name="check" className="ok" />
      )}
      {time}
    </span>
  );
}

function CommandCard({ item, open, onOpen }: { item: ToolItem; open?: boolean; onOpen: (open: boolean) => void }) {
  const call = item.call as Extract<Call, { name: "Bash" }>;
  const state = callState(item);
  const expanded = state !== "running" && (open ?? state === "failed");
  const [first, ...more] = call.command.split("\n");
  return (
    <div className={`cmd is-${state}`}>
      <button type="button" className="cmd-head" aria-expanded={expanded} disabled={state === "running"} onClick={() => onOpen(!expanded)}>
        <Icon name="terminal" className="cmd-icon" />
        <span className="cmd-line">
          <span className="cmd-prompt">$</span> {first}
        </span>
        {more.length > 0 && <span className="cmd-more">+{plural(more.length, "line")}</span>}
        <CommandStatus item={item} state={state} />
        {state !== "running" && <Icon name="chevron-down" className="chev" />}
      </button>
      {expanded && (
        <div className="cmd-body">
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

/** Lines of a diff shown before a click shows all. */
const PREVIEW = 12;

function Delta({ added, removed }: { added: number; removed: number }) {
  return (
    <span className="delta">
      {added > 0 && <span className="plus">+{added}</span>}
      {removed > 0 && <span className="minus">−{removed}</span>}
    </span>
  );
}

function FileDiffView({
  diff,
  state,
  full,
  onFull,
  project,
  thread,
  writing,
}: {
  diff: FileDiff;
  state: CallState | DraftState;
  full: boolean;
  onFull?: (full: boolean) => void;
  project: string;
  thread?: number;
  /** What a call being written says it is doing: its last lines show. */
  writing?: string;
}) {
  const lines = diff.lines;
  const from = full ? 0 : writing ? Math.max(0, lines.length - PREVIEW) : 0;
  const shown = full ? lines : lines.slice(from, from + PREVIEW);
  const hidden = lines.length - shown.length;
  // the lines highlighted together, what was and what is, in the file's language
  const html = highlightLines(
    shown.map((l) => (l.kind === "gap" ? "" : l.text)),
    languageOf(diff.path),
  );
  return (
    <div className="fdiff">
      <div className="fdiff-head">
        <Icon name={diff.created ? "new-file" : diff.deleted ? "trash" : "file"} className="fdiff-icon" />
        <button type="button" className="fdiff-name" title={`Open ${diff.path}`} onClick={() => openFile(diff.path, project, diff.at)}>
          <FileName path={diff.path} />
        </button>
        {diff.created && <span className="tag">new</span>}
        {diff.deleted && <span className="tag">deleted</span>}
        <span className="spacer" />
        {writing ? (
          <span className="writing">
            {state === "writing" && <Icon name="loading" spin />} {writing}
          </span>
        ) : (
          <Delta added={diff.added} removed={diff.removed} />
        )}
        {state === "running" && <Icon name="loading" spin />}
        {state === "failed" && <span className="tag err">failed</span>}
        {state === "refused" && <span className="tag warn">not allowed</span>}
        {state === "ok" && thread !== undefined && (
          <IconButton icon="diff" title="Open the change in the diff" className="reveal" onClick={() => openDiff(thread, diff.path, project)} />
        )}
      </div>
      {shown.length > 0 && (
        <div className="fdiff-body">
          <div className="fdiff-lines">
            {writing && hidden > 0 && <div className="dl gap">⋯ {plural(hidden, "line")} above</div>}
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
      )}
      {onFull && !writing && hidden > 0 && (
        <button type="button" className="fdiff-more" onClick={() => onFull(true)}>
          Show {plural(hidden, "more line", "more lines")}
        </button>
      )}
      {onFull && !writing && full && lines.length > PREVIEW && (
        <button type="button" className="fdiff-more" onClick={() => onFull(false)}>
          Show less
        </button>
      )}
    </div>
  );
}

function firstLines(text: string, n = 3): string {
  return text.replace(/^\[(error|refused)\]\s*/, "").split("\n").slice(0, n).join("\n");
}

function ChangeCard({
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
    <div className={`change is-${state}`}>
      {files.map((f) => (
        <FileDiffView key={f.path} diff={f} state={state} full={Boolean(open)} onFull={onOpen} project={project} thread={thread} />
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
      return <CommandCard item={item} open={open} onOpen={onOpen} />;
    case "change":
      return <ChangeCard item={item} open={open} onOpen={onOpen} project={project} thread={thread} />;
    default:
      return <ToolLine item={item} open={open} onOpen={onOpen} project={project} />;
  }
});

/** The call a draft makes so far, for the tools whose drafts show as changes. */
function draftCall(d: Draft): Call | undefined {
  const f = (name: string) => d.fields.find(([n]) => n === name)?.[1];
  switch (d.tool) {
    case "Edit":
      return f("path") === undefined ? undefined : { name: "Edit", path: f("path")!, old: f("old") ?? "", new: f("new") ?? "", replaceAll: false };
    case "Write":
      return f("path") === undefined ? undefined : { name: "Write", path: f("path")!, content: f("content") ?? "" };
    case "ApplyPatch":
      return { name: "ApplyPatch", patch: f("patch") ?? "" };
    default:
      return undefined;
  }
}

function draftSummary(d: Draft): string {
  const f = (name: string) => d.fields.find(([n]) => n === name)?.[1];
  return (f("path") ?? f("pattern") ?? f("query") ?? f("command") ?? "").split("\n")[0]!;
}

const DRAFT_SAYS: Record<DraftState, string> = { writing: "Writing…", written: "About to run", void: "Not run" };

/** A call the model writes, before it runs. */
export const DraftView = memo(function DraftView({ draft, state, project }: { draft: Draft; state: DraftState; project: string }) {
  const says = DRAFT_SAYS[state];
  const kind = shape(draft.tool);
  if (kind === "change") {
    const call = draftCall(draft);
    const files = call ? diffsOf({ kind: "tool", id: "draft", shown: draft.tool ?? "", raw: {}, call }) : [];
    return (
      <div className={`change draft is-${state}`}>
        {files.length ? (
          files.map((f) => <FileDiffView key={f.path} diff={f} state={state} full={false} project={project} writing={says} />)
        ) : (
          <div className="fdiff">
            <div className="fdiff-head">
              <Icon name="loading" spin className="fdiff-icon" />
              <span className="writing">{says}</span>
            </div>
          </div>
        )}
      </div>
    );
  }
  if (kind === "command") {
    const command = draftSummary(draft);
    return (
      <div className={`cmd draft is-${state}`}>
        <div className="cmd-head">
          <Icon name="terminal" className="cmd-icon" />
          <span className="cmd-line">
            <span className="cmd-prompt">$</span> {command}
          </span>
          <span className="cmd-status">
            {state === "writing" && <Icon name="loading" spin />} <span className="muted">{says}</span>
          </span>
        </div>
      </div>
    );
  }
  return (
    <div className={`tline draft is-${state}`}>
      <div className="tline-head">
        <span className="tline-toggle">
          <Icon name={state === "writing" ? "loading" : "tools"} spin={state === "writing"} className="tline-icon" />
          <span className="tline-verb">{draft.tool ?? "Call"}</span>
          <span className="tline-target">{draftSummary(draft)}</span>
          <span className="tline-meta">{says}</span>
        </span>
      </div>
    </div>
  );
});
