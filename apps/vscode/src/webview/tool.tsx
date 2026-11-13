// A call in the thread: a line with its tool, what it works on and how it ended, which
// opens to its result. A file it read or changed opens in the editor, a change in the
// editor's diff. A call the model is still writing shows its code as it comes.

import { memo } from "react";

import { body, header, summary } from "./model/format.ts";
import type { ToolItem } from "./model/model.ts";
import type { Draft, DraftState } from "./model/store.ts";
import { openDiff, openFile } from "./host.ts";

/** VS Code's icon of each tool. */
function icon(tool: string | undefined): string {
  switch (tool) {
    case "Read":
      return "file";
    case "Edit":
      return "edit";
    case "Write":
      return "new-file";
    case "ApplyPatch":
      return "diff";
    case "Bash":
      return "terminal";
    case "Grep":
      return "search";
    case "Glob":
      return "files";
    case "Task":
      return "organization";
    case "Mcp":
      return "plug";
    case "ToolSearch":
      return "search-fuzzy";
    case "Skill":
      return "book";
    default:
      return tool?.startsWith("mcp__") ? "plug" : "tools";
  }
}

function label(tool: string | undefined): string {
  if (tool === "ApplyPatch") return "Patch";
  if (tool === "ToolSearch") return "Find";
  if (tool === "Mcp" || tool?.startsWith("mcp__")) return "MCP";
  return tool ?? "Call";
}

function Status({ item }: { item: ToolItem }) {
  const r = item.result;
  if (!r) return <span className="codicon codicon-loading codicon-modifier-spin status" title="running" />;
  if (r.outcome === "refused") return <span className="status warn">refused</span>;
  if (r.outcome === "error") return <span className="status err">error</span>;
  if (r.exit !== undefined && r.exit !== 0) return <span className="status err">exit {r.exit}</span>;
  if (r.text.startsWith("[shell: timed out") || r.text.startsWith("[shell: killed")) return <span className="status err">killed</span>;
  return null;
}

/** What it worked on, on the right: the tool's header or the time a command took. */
function meta(item: ToolItem): string | undefined {
  const r = item.result;
  if (r?.exit !== undefined && r.seconds !== undefined) return `${r.seconds.toFixed(1)} s`;
  return r?.outcome === "ok" ? header(item) : undefined;
}

/** The lines of an edit: what it took out and what it put in. */
function EditLines({ old, now }: { old: string; now: string }) {
  const cut = (t: string) => t.replace(/\n$/, "").split("\n");
  return (
    <pre className="lines">
      {old &&
        cut(old).map((l, i) => (
          <div key={`d${i}`} className="del">
            {`- ${l}`}
          </div>
        ))}
      {now &&
        cut(now).map((l, i) => (
          <div key={`a${i}`} className="add">
            {`+ ${l}`}
          </div>
        ))}
    </pre>
  );
}

function Output({ text }: { text: string }) {
  return text.trim() ? <pre className="output">{text}</pre> : <p className="output muted">no output</p>;
}

function Body({ item }: { item: ToolItem }) {
  const call = item.call;
  if (item.result && item.result.outcome !== "ok") return <Output text={item.result.text} />;
  if (item.result?.images?.length) {
    return (
      <div className="pictures">
        {item.result.images.map((p, i) => (
          <img key={i} src={`data:${p.mime};base64,${p.data}`} alt={`image ${i + 1} the call read`} />
        ))}
      </div>
    );
  }
  if (call?.name === "Edit") return <EditLines old={call.old} now={call.new} />;
  if (call?.name === "Write") return <Output text={call.content} />;
  if (call?.name === "ApplyPatch") return <Output text={call.patch} />;
  if (call?.name === "Mcp") return <Output text={`${call.arguments}\n\n${body(item)}`} />;
  if (!call) return <Output text={JSON.stringify(item.raw, null, 2)} />;
  return <Output text={body(item)} />;
}

/** The file a call worked on, and the line it started at. */
function target(item: ToolItem): { path: string; line?: number; changed: boolean } | undefined {
  const c = item.call;
  if (c?.name === "Read") return { path: c.path, line: c.offset && c.offset > 1 ? c.offset : undefined, changed: false };
  const applied =
    item.result?.outcome === "ok" &&
    ((c?.name === "Edit" && item.result.text.startsWith("[edit: applied")) || (c?.name === "Write" && item.result.text.startsWith("[write: ")));
  if (c?.name === "Edit" || c?.name === "Write") return { path: c.path, changed: Boolean(applied) };
  return undefined;
}

export const ToolRow = memo(function ToolRow({
  item,
  open,
  onOpen,
  project,
  thread,
}: {
  item: ToolItem;
  open: boolean;
  onOpen: (open: boolean) => void;
  project: string;
  thread?: number;
}) {
  const file = target(item);
  const tool = item.call?.name ?? item.shown;
  const shownMeta = meta(item);
  return (
    <div className={`tool ${item.result ? "" : "running"}`}>
      <div className="tool-head">
        <button type="button" className="tool-toggle" aria-expanded={open} onClick={() => onOpen(!open)}>
          <span className={`codicon codicon-${icon(tool)}`} />
          <span className="tool-name">{label(tool)}</span>
          <span className="tool-summary">{item.call ? summary(item.call) : `${item.shown} ${JSON.stringify(item.raw)}`}</span>
          {shownMeta && <span className="tool-meta">{shownMeta}</span>}
          <Status item={item} />
        </button>
        {file?.changed && thread !== undefined && (
          <button type="button" className="icon-button" title="Open the change in the diff" onClick={() => openDiff(thread, file.path, project)}>
            <span className="codicon codicon-diff" />
          </button>
        )}
        {file && (
          <button type="button" className="icon-button" title={`Open ${file.path}`} onClick={() => openFile(file.path, project, file.line)}>
            <span className="codicon codicon-go-to-file" />
          </button>
        )}
      </div>
      {open && (
        <div className="tool-body">
          <Body item={item} />
        </div>
      )}
    </div>
  );
});

/** Lines of a long draft shown while it comes: its end. */
const DRAFT_LINES = 14;

function draftText(d: Draft): string {
  const f = (name: string) => d.fields.find(([n]) => n === name)?.[1];
  switch (d.tool) {
    case "Write":
      return f("content") ?? "";
    case "Edit":
      return [f("old") && `- ${f("old")}`, f("new") && `+ ${f("new")}`].filter(Boolean).join("\n");
    case "ApplyPatch":
      return f("patch") ?? "";
    case "Bash":
      return `$ ${f("command") ?? f("argv") ?? ""}`;
    default:
      return d.fields
        .filter(([n]) => n !== "path")
        .map(([n, v]) => `${n}: ${v}`)
        .join("\n");
  }
}

function draftSummary(d: Draft): string {
  const f = (name: string) => d.fields.find(([n]) => n === name)?.[1];
  const patched = f("patch")?.match(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/m)?.[1];
  return (f("path") ?? patched ?? f("pattern") ?? f("query") ?? f("command") ?? "").split("\n")[0]!;
}

/** A call the model writes, before it runs. */
export const DraftRow = memo(function DraftRow({ draft, state }: { draft: Draft; state: DraftState }) {
  const text = draftText(draft);
  const lines = text.split("\n");
  const tail = lines.slice(-DRAFT_LINES).join("\n");
  return (
    <div className={`tool draft ${state}`}>
      <div className="tool-head">
        <span className="tool-toggle">
          <span className={`codicon codicon-${state === "writing" ? "loading codicon-modifier-spin" : icon(draft.tool)}`} />
          <span className="tool-name">{label(draft.tool)}</span>
          <span className="tool-summary">{draftSummary(draft)}</span>
          <span className="tool-meta">{state === "writing" ? "writing…" : state === "void" ? "not run" : "about to run"}</span>
        </span>
      </div>
      {tail.trim() && state !== "void" && (
        <pre className="output">
          {lines.length > DRAFT_LINES && <span className="muted">{`… ${lines.length - DRAFT_LINES} lines above\n`}</span>}
          {tail}
        </pre>
      )}
    </div>
  );
});
