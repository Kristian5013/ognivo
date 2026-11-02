import { type ReactNode, useCallback, useMemo, useSyncExternalStore } from "react";

import { count, duration } from "../format.ts";
import type { StoredMessage } from "../model.ts";
import type { ThreadStore } from "../store.ts";
import profiles from "../profiles.json";

// What a generation was asked and gave, exactly: its request is the prefix of the stored
// conversation whose sha256 is the prompt_hash it recorded, and its answer the message
// that history kept after it (scripts/fixtures.mjs). With a core the same comes from it.

const TOKENS = Object.fromEntries(
  Object.entries(profiles as Record<string, { tokens: { framing: string[]; other: string[] } }>).map(
    ([id, p]) => [id, p.tokens],
  ),
);
/** U+2060, the guard's joiner (docs/architecture.md §5.3). */
const JOINER = "⁠";

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** A matcher of the profile's control tokens, whole or broken by the joiner after their
 * first character, the longest first. */
function matcher(profile: string): { re: RegExp; framing: Set<string> } | undefined {
  const t = TOKENS[profile];
  if (!t) return undefined;
  const all = [...t.framing, ...t.other].sort((a, b) => b.length - a.length);
  const alt = all.map((s) => `${escape(s.slice(0, 1))}${JOINER}*${escape(s.slice(1))}`).join("|");
  return { re: new RegExp(alt, "g"), framing: new Set(t.framing) };
}

/** Text with the profile's control tokens marked: framing ones (they frame the model's own
 * messages and calls) and the others; a joiner the guard put in shows as `⁠·`. */
function Marked({ text, profile }: { text: string; profile: string }) {
  const m = useMemo(() => matcher(profile), [profile]);
  const parts = useMemo(() => {
    if (!m || !text) return [text];
    const out: ReactNode[] = [];
    let last = 0;
    for (const hit of text.matchAll(m.re)) {
      const at = hit.index ?? 0;
      if (at > last) out.push(text.slice(last, at));
      const whole = hit[0].replaceAll(JOINER, "");
      const broken = hit[0] !== whole;
      out.push(
        <span
          key={at}
          title={broken ? "broken by the guard's joiner" : "control token"}
          className={`rounded-sm px-0.5 ${m.framing.has(whole) ? "bg-spark/20 text-spark" : "bg-raised text-warn"}`}
        >
          {broken ? `${whole.slice(0, 1)}·${whole.slice(1)}` : whole}
        </span>,
      );
      last = at + hit[0].length;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  }, [m, text]);
  return <>{parts}</>;
}

function Block({ text, profile }: { text: string; profile: string }) {
  return (
    <pre className="max-h-64 overflow-auto font-mono text-[11.5px] leading-[18px] break-words whitespace-pre-wrap">
      <Marked text={text} profile={profile} />
    </pre>
  );
}

function Message({ m, fresh, profile }: { m: StoredMessage; fresh: boolean; profile: string }) {
  return (
    <div className={`border-l-2 py-1 pl-2 ${fresh ? "border-spark" : "border-border"}`}>
      <div className="text-[11px] text-muted-foreground">
        <span className="tracking-wide uppercase">{m.role}</span>
        {m.tool_call_id ? <span className="font-mono"> · {m.tool_call_id}</span> : null}
        {fresh ? " · new" : ""}
      </div>
      {m.reasoning !== undefined && (
        <details>
          <summary className="cursor-pointer text-[12px] text-muted-foreground">
            reasoning, {m.reasoning.length} characters
          </summary>
          <Block text={m.reasoning} profile={profile} />
        </details>
      )}
      {m.content && <Block text={m.content} profile={profile} />}
      {m.tool_calls?.map((c) => (
        <div key={c.id} className="mt-1">
          <span className="font-mono text-[11px] text-spark">
            {c.name} · {c.id}
          </span>
          <Block text={c.arguments} profile={profile} />
        </div>
      ))}
    </div>
  );
}

function Section({ title, open, children }: { title: string; open?: boolean; children: ReactNode }) {
  return (
    <details open={open} className="border-b border-border">
      <summary className="cursor-pointer px-3 py-1.5 text-[12px] font-medium select-none">{title}</summary>
      <div className="flex flex-col gap-1.5 px-3 pb-3">{children}</div>
    </details>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 truncate">{children}</dd>
    </>
  );
}

/** One generation, as exactly as the record allows: the numbers, the request, the answer
 * and the results that went back. */
export function Inspector({
  store,
  generation,
  onGeneration,
}: {
  store: ThreadStore;
  generation?: number;
  onGeneration: (generation: number) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  useSyncExternalStore(subscribe, () => store.listVersion);
  const gens = store.generations;
  const g = Math.min(generation ?? gens.length - 1, gens.length - 1);
  const gen = gens[g];
  if (!gen) return <p className="m-auto px-6 text-center text-muted-foreground">No generation yet.</p>;

  const profile = store.info.profile;
  const req = gen.request;
  const conversation = req ? (store.info.conversations[req.conversation] ?? []) : [];
  const asked = req ? conversation.slice(0, req.messages) : [];
  const before = gens
    .slice(0, g)
    .reverse()
    .find((p) => p.request?.conversation === req?.conversation)?.request?.messages;
  // a usable generation's answer is the message history kept after its request; then the
  // results that went back, up to the next request
  const next = gens.slice(g + 1).find((p) => p.request?.conversation === req?.conversation)?.request?.messages;
  const answer = req && conversation[req.messages]?.role === "assistant" ? conversation[req.messages] : undefined;
  const results = answer && req ? conversation.slice(req.messages + 1, next ?? conversation.length) : [];
  const unused = store.itemsOf(g).filter((i) => i.kind === "void");
  const processed = Math.max(0, gen.prompt - gen.cached);

  const nav = "rounded border border-border px-2 hover:bg-raised disabled:opacity-40";
  return (
    <div className="min-h-0 flex-1 overflow-y-auto text-[12px]">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <button type="button" className={nav} disabled={g === 0} onClick={() => onGeneration(g - 1)}>
          ‹
        </button>
        <span>
          Generation {g + 1} of {gens.length}
        </span>
        <button
          type="button"
          className={nav}
          disabled={g === gens.length - 1}
          onClick={() => onGeneration(g + 1)}
        >
          ›
        </button>
        <span className={`ml-auto ${gen.outcome === "malformed" ? "text-warn" : "text-muted-foreground"}`}>
          {gen.outcome}
          {gen.finish ? ` · ${gen.finish}` : ""}
        </span>
      </div>
      <dl className="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-0.5 border-b border-border px-3 py-2">
        <Fact label="prompt">
          {count(gen.prompt)} tokens, {count(gen.cached)} from the cache, {count(processed)} processed
        </Fact>
        <Fact label="prefill">
          {duration(gen.promptMs)}
          {gen.promptMs ? ` · ${Math.round(processed / (gen.promptMs / 1000))} tok/s` : ""}
        </Fact>
        <Fact label="generated">
          {count(gen.completion)} tokens in {duration(gen.generationMs)}
          {gen.generationMs ? ` · ${Math.round(gen.completion / (gen.generationMs / 1000))} tok/s` : ""}
        </Fact>
        {gen.seed !== undefined && <Fact label="seed">{gen.seed}</Fact>}
        {gen.promptHash && <Fact label="prompt hash">{gen.promptHash.replace(/^sha256:/, "").slice(0, 16)}</Fact>}
        <Fact label="request">
          {req
            ? `${asked.length} messages, ${store.info.tools.length} tools: exact, its sha256 matched`
            : "not in the record"}
        </Fact>
      </dl>
      {req && (
        <Section title={`Request: ${asked.length} messages${before !== undefined ? `, ${asked.length - before} new` : ""}`}>
          {asked.map((m, i) => (
            <Message key={i} m={m} fresh={before !== undefined && i >= before} profile={profile} />
          ))}
        </Section>
      )}
      {req && (
        <Section title={`Tools: ${store.info.tools.map((t) => t.name).join(", ")}`}>
          {store.info.tools.map((t) => (
            <div key={t.name}>
              <span className="font-mono text-spark">{t.name}</span>
              <Block text={JSON.stringify(t.parameters, null, 1)} profile={profile} />
            </div>
          ))}
        </Section>
      )}
      <Section title="Answer" open>
        {answer ? (
          <Message m={answer} fresh={false} profile={profile} />
        ) : unused.length ? (
          unused.map((v) =>
            v.kind === "void" ? (
              <div key={v.id}>
                <p className="text-warn">
                  {v.why === "empty"
                    ? "It stopped inside its reasoning and is not in the history."
                    : "Its calls could not be used; it is not in the history."}
                </p>
                {v.calls.map((c, i) => (
                  <Block key={i} text={`${c.name} ${c.arguments}`} profile={profile} />
                ))}
              </div>
            ) : null,
          )
        ) : (
          <p className="text-muted-foreground">Not in the record.</p>
        )}
      </Section>
      {results.length > 0 && (
        <Section title={`Sent back: ${results.length} messages`} open>
          {results.map((m, i) => (
            <Message key={i} m={m} fresh={false} profile={profile} />
          ))}
        </Section>
      )}
    </div>
  );
}
