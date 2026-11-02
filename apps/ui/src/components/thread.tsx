import * as Collapsible from "@radix-ui/react-collapsible";
import { useMemo, useState } from "react";
import { Virtuoso } from "react-virtuoso";
import { Streamdown } from "streamdown";

import { ending, seconds } from "../format.ts";
import type { Item, Thread, Turn, VoidItem } from "../model.ts";
import { ToolCard } from "./tool.tsx";

type Row =
  | { kind: "user"; key: string; turn: Turn }
  | { kind: "item"; key: string; item: Item }
  | { kind: "end"; key: string; turn: Turn };

function rows(thread: Thread): Row[] {
  return thread.turns.flatMap((turn): Row[] => [
    { kind: "user", key: `u${turn.index}`, turn },
    ...turn.items.map((item): Row => ({ kind: "item", key: item.id, item })),
    ...(turn.end ? [{ kind: "end", key: `e${turn.index}`, turn } as const] : []),
  ]);
}

function Chevron({ open }: { open: boolean }) {
  return (
    <span aria-hidden className={`inline-block transition-transform ${open ? "rotate-90" : ""}`}>
      ›
    </span>
  );
}

function Reasoning({
  text,
  ms,
  open,
  onOpenChange,
}: {
  text: string;
  ms?: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Collapsible.Root open={open} onOpenChange={onOpenChange}>
      <Collapsible.Trigger className="flex cursor-pointer items-center gap-2 text-muted-foreground hover:text-foreground">
        <Chevron open={open} />
        {ms ? `Thought for ${seconds(ms)}` : "Thought"}
      </Collapsible.Trigger>
      <Collapsible.Content>
        {open && (
          <p className="mt-1.5 border-l-2 border-border pl-3 leading-5 whitespace-pre-wrap text-muted-foreground">
            {text}
          </p>
        )}
      </Collapsible.Content>
    </Collapsible.Root>
  );
}

function Void({ item }: { item: VoidItem }) {
  if (item.why === "empty") {
    return <p className="text-warn">The model stopped inside its reasoning: nothing ran.</p>;
  }
  return (
    <div className="rounded-md border border-warn/40 px-3 py-2">
      <p className="text-warn">The calls of this generation could not be used, so none ran.</p>
      {item.calls.map((c, i) => (
        <pre key={i} className="mt-1 font-mono text-[12px] break-all whitespace-pre-wrap text-muted-foreground">
          {c.name} {c.arguments}
        </pre>
      ))}
    </div>
  );
}

/** A recorded session, turn by turn: only what is on screen is rendered. */
export function ThreadView({ thread }: { thread: Thread }) {
  const data = useMemo(() => rows(thread), [thread]);
  // changes are what the user checks first: they start open; `?open=all` opens everything
  // (screenshots, perf runs)
  const [open, setOpen] = useState<ReadonlySet<string>>(() => {
    const all = new URLSearchParams(location.search).get("open") === "all";
    return new Set(
      thread.turns.flatMap((t) =>
        t.items
          .filter((i) => all || (i.kind === "tool" && i.call?.name === "Edit"))
          .map((i) => i.id),
      ),
    );
  });
  const toggle = (id: string) => (on: boolean) =>
    setOpen((s) => {
      const next = new Set(s);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  const item = (i: Item) => {
    switch (i.kind) {
      case "reasoning":
        return (
          <Reasoning
            text={i.text}
            ms={i.generation === undefined ? undefined : thread.generations[i.generation]?.generationMs}
            open={open.has(i.id)}
            onOpenChange={toggle(i.id)}
          />
        );
      case "message":
        return <Streamdown className="leading-6">{i.text}</Streamdown>;
      case "tool":
        return <ToolCard item={i} open={open.has(i.id)} onOpenChange={toggle(i.id)} />;
      case "notice":
        return (
          <p className="text-[12px] leading-5 text-muted-foreground">
            <span className="mr-2 font-medium text-spark">ognivo</span>
            {i.text.replace(/^\[ognivo\]\s*/, "")}
          </p>
        );
      case "void":
        return <Void item={i} />;
    }
  };

  const row = (r: Row) => {
    switch (r.kind) {
      case "user":
        return (
          <div className="mt-5 rounded-lg bg-raised px-4 py-3">
            <Streamdown className="leading-6">{r.turn.user}</Streamdown>
          </div>
        );
      case "item":
        return item(r.item);
      case "end":
        return (
          <div className="mt-2 mb-6 flex items-center gap-3 text-[12px] text-muted-foreground">
            <span className="h-px flex-1 bg-border" />
            <span className={r.turn.end === "answer" ? undefined : "text-warn"}>
              Turn {r.turn.index} {ending(r.turn.end)}
              {r.turn.why ? `: ${r.turn.why}` : ""}
            </span>
            <span className="h-px flex-1 bg-border" />
          </div>
        );
    }
  };

  return (
    <Virtuoso
      className="min-h-0 flex-1"
      data={data}
      computeItemKey={(_, r) => r.key}
      increaseViewportBy={800}
      itemContent={(_, r) => <div className="mx-auto max-w-3xl px-6 py-1">{row(r)}</div>}
    />
  );
}
