import * as Collapsible from "@radix-ui/react-collapsible";

import { body, header, summary } from "../format.ts";
import type { ToolItem } from "../model.ts";
import { EditDiff } from "./diff.tsx";

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
