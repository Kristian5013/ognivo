import * as Collapsible from "@radix-ui/react-collapsible";

import { body, header, summary } from "../format.ts";
import type { ToolItem } from "../model.ts";
import type { Draft, DraftState } from "../store.ts";
import { EditDiff } from "./diff.tsx";
import { Follow } from "./follow.tsx";

function Status({ item }: { item: ToolItem }) {
  const r = item.result;
  const [tone, label] = !r
    ? ["text-muted-foreground", "running"]
    : r.outcome === "refused"
      ? ["text-warn", "refused"]
      : r.outcome === "error"
        ? ["text-err", "error"]
        : r.exit !== undefined && r.exit !== 0
          ? ["text-err", `exit ${r.exit}`]
          : r.text.startsWith("[shell: timed out") || r.text.startsWith("[shell: killed")
            ? ["text-err", "killed"]
            : [undefined, undefined];
  return label ? <span className={`shrink-0 font-medium ${tone}`}>{label}</span> : null;
}

/** What the call worked on, on the right of its line: the tool's header or the exit. */
function Meta({ item }: { item: ToolItem }) {
  const r = item.result;
  const text =
    r?.exit !== undefined ? `${r.seconds?.toFixed(1)} s` : r?.outcome === "ok" ? header(item) : undefined;
  return text ? <span className="hidden shrink-0 text-muted-foreground md:inline">{text}</span> : null;
}

function Text({ text }: { text: string }) {
  return text.trim() ? (
    <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words px-3 py-2 font-mono text-[12px] leading-5 text-muted-foreground">
      {text}
    </pre>
  ) : (
    <p className="px-3 py-2 text-muted-foreground italic">no output</p>
  );
}

/** JSON as it reads best, or the text as it came. */
function pretty(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

function Body({ item }: { item: ToolItem }) {
  const call = item.call;
  if (item.result && item.result.outcome !== "ok") return <Text text={item.result.text} />;
  if (call?.name === "Edit") return <EditDiff path={call.path} before={call.old} after={call.new} />;
  if (call?.name === "Write") return <Text text={call.content} />;
  if (call?.name === "ApplyPatch") return <Text text={call.patch} />;
  if (call?.name === "Mcp") return <Text text={`${pretty(call.arguments)}\n\n${body(item)}`} />;
  if (!call) return <Text text={JSON.stringify(item.raw, null, 2)} />;
  return <Text text={body(item)} />;
}

/** One call: a line with what it worked on and how it ended; opened, its result. */
export function ToolCard({
  item,
  open,
  onOpenChange,
}: {
  item: ToolItem;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Collapsible.Root
      open={open}
      onOpenChange={onOpenChange}
      className={`rounded-md border border-border bg-panel ${item.result ? "" : "running"}`}
    >
      <Collapsible.Trigger
        style={{ paddingBlock: "var(--line-py)" }}
        className="flex w-full min-w-0 cursor-pointer items-center gap-3 px-3 text-left hover:bg-raised"
      >
        <span className="w-11 shrink-0 font-mono text-[11px] tracking-wide text-spark uppercase">
          {item.call?.name === "ApplyPatch"
            ? "Patch"
            : item.call?.name === "Mcp"
              ? "MCP"
              : item.call?.name === "ToolSearch"
                ? "Find"
                : (item.call?.name ?? item.shown)}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-[12px]">
          {item.call ? summary(item.call) : `${item.shown} ${JSON.stringify(item.raw)}`}
        </span>
        <Meta item={item} />
        <Status item={item} />
      </Collapsible.Trigger>
      <Collapsible.Content className="border-t border-border">
        {open && <Body item={item} />}
      </Collapsible.Content>
    </Collapsible.Root>
  );
}

/** How a call's tool is labelled on its line. */
function label(tool: string): string {
  if (tool === "ApplyPatch") return "Patch";
  if (tool === "ToolSearch") return "Find";
  if (tool.startsWith("mcp__") || tool === "mcp_call") return "MCP";
  return tool;
}

/** A line of a call being written, with how it reads: added, removed, or a patch's own. */
interface Line {
  text: string;
  tone?: "add" | "del" | "dim";
}

/** Lines shown of a long draft while it comes. */
const DRAFT_LINES = 400;

function lines(text: string, tone?: Line["tone"], mark = ""): Line[] {
  return text
    .replace(/\n$/, "")
    .split("\n")
    .map((t) => ({ text: `${mark}${t}`, tone }));
}

/** What a call being written shows below its line: the code it writes, the text it
 * replaces and the text it puts in, the patch, the command. */
function draftLines(d: Draft): Line[] {
  const f = (name: string) => d.fields.find(([n]) => n === name)?.[1];
  switch (d.tool) {
    case "Write":
      return lines(f("content") ?? "");
    case "Edit":
      return [...lines(f("old") ?? "", "del", "- "), ...lines(f("new") ?? "", "add", "+ ")].filter(
        (l) => l.text !== "- " && l.text !== "+ ",
      );
    case "ApplyPatch":
      return lines(f("patch") ?? "").map((l) => ({
        ...l,
        tone: l.text.startsWith("+")
          ? "add"
          : l.text.startsWith("-")
            ? "del"
            : l.text.startsWith("***") || l.text.startsWith("@@")
              ? "dim"
              : undefined,
      }));
    case "Bash": {
      const command = f("command") ?? f("argv");
      return command ? lines(command, undefined, "$ ") : [];
    }
    case "Read":
    case "Grep":
    case "Glob":
      return [];
    default:
      return d.fields.filter(([n]) => n !== "path").map(([n, v]) => ({ text: `${n}: ${v}` }));
  }
}

/** What the call works on, on its line: its file, its pattern, its command. */
function draftSummary(d: Draft): string {
  const f = (name: string) => d.fields.find(([n]) => n === name)?.[1];
  const patched = f("patch")?.match(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/m)?.[1];
  return (f("path") ?? patched ?? f("pattern") ?? f("query") ?? f("command") ?? f("argv") ?? "").split("\n")[0]!;
}

const TONE: Record<NonNullable<Line["tone"]>, string> = {
  add: "text-ok",
  del: "text-err",
  dim: "text-muted-foreground",
};

/** A call the model is writing: its tool and file on a line with a circle that turns,
 * and the code below as it comes. Its item takes the row once the generation ended. */
export function DraftCard({ draft, state }: { draft: Draft; state: DraftState }) {
  const shown = draftLines(draft);
  const cut = Math.max(0, shown.length - DRAFT_LINES);
  const size = draft.fields.reduce((n, [, v]) => n + v.length, 0);
  return (
    <div className={`rounded-md border border-border bg-panel ${state === "writing" ? "running" : ""}`}>
      <div style={{ paddingBlock: "var(--line-py)" }} className="flex min-w-0 items-center gap-3 px-3">
        <span className="w-11 shrink-0 font-mono text-[11px] tracking-wide text-spark uppercase">
          {label(draft.tool ?? "Call")}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-[12px]">{draftSummary(draft)}</span>
        {state === "writing" ? (
          <>
            <span className="hidden shrink-0 text-muted-foreground md:inline">writing…</span>
            <span className="spinner" role="status" aria-label="The model writes this call" />
          </>
        ) : state === "written" ? (
          <span className="shrink-0 text-muted-foreground">waiting to run</span>
        ) : (
          <span className="shrink-0 font-medium text-warn">not run</span>
        )}
      </div>
      {state !== "void" && shown.length > 0 && (
        <Follow
          content={size}
          label="The call as the model writes it"
          className="max-h-64 border-t border-border px-3 py-2"
        >
          <pre className="font-mono text-[12px] leading-5 break-words whitespace-pre-wrap">
            {cut > 0 && <span className="text-muted-foreground">{`… ${cut} lines above\n`}</span>}
            {shown.slice(cut).map((l, i) => (
              <span key={i + cut} className={l.tone ? TONE[l.tone] : undefined}>
                {`${l.text}\n`}
              </span>
            ))}
          </pre>
        </Follow>
      )}
    </div>
  );
}
