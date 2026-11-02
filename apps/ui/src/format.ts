import type { Call, ToolItem } from "./model.ts";

export function seconds(ms: number): string {
  return ms < 10_000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms / 1000)} s`;
}

export function count(n: number): string {
  return n < 10_000 ? String(n) : `${(n / 1000).toFixed(n < 100_000 ? 1 : 0)}k`;
}

export function percent(part: number, whole: number): string {
  return whole ? `${Math.round((part / whole) * 100)}%` : "—";
}

/** The line a call gets in the thread: what it works on. */
export function summary(call: Call): string {
  switch (call.name) {
    case "Read":
      return call.offset && call.offset > 1 ? `${call.path} from line ${call.offset}` : call.path;
    case "Edit":
      return call.replaceAll ? `${call.path} · every occurrence` : call.path;
    case "Write": {
      // as wc -l, but a last line without its newline counts too
      const n = call.content ? call.content.split("\n").length - (call.content.endsWith("\n") ? 1 : 0) : 0;
      return `${call.path} · ${n} ${n === 1 ? "line" : "lines"}`;
    }
    case "Bash":
      return `$ ${call.command.split("\n")[0]}`;
    case "Grep":
      return `/${call.pattern}/${call.path ? ` in ${call.path}` : ""}${call.glob ? ` · ${call.glob}` : ""}`;
    case "Glob":
      return `${call.pattern}${call.path ? ` in ${call.path}` : ""}`;
  }
}

/** The tool's own first line, `[read: … — lines 1-11 of 11 lines, …]`, without its name. */
export function header(item: ToolItem): string | undefined {
  const first = item.result?.text.split("\n", 1)[0] ?? "";
  const m = /^\[(?:read|edit|write|grep|glob): (.*)\]$/.exec(first);
  return m?.[1];
}

/** A result without the header line, which the card shows itself. */
export function body(item: ToolItem): string {
  const text = item.result?.text ?? "";
  return /^\[(read|edit|write|grep|glob|shell)[: ]/.test(text)
    ? text.slice(text.indexOf("\n") + 1 || text.length)
    : text;
}

/** How a turn ended, as a person would say it. */
export function ending(end: string | undefined): string {
  switch (end) {
    case "answer":
      return "answered";
    case "step_limit":
      return "stopped at the step limit";
    case "time_limit":
      return "stopped at the time limit";
    case "stuck":
      return "stopped: the model was stuck";
    case "error":
      return "failed";
    default:
      return end ?? "running";
  }
}

/** Lines a change adds and removes, by a longest common subsequence of its lines; plain
 * counts when the two sides are too long for one. */
export function lineDelta(before: string, after: string): { added: number; removed: number } {
  const a = before ? before.split("\n") : [];
  const b = after ? after.split("\n") : [];
  if (a.length * b.length > 250_000) return { added: b.length, removed: a.length };
  let prev = new Array<number>(b.length + 1).fill(0);
  for (const line of a) {
    const row = new Array<number>(b.length + 1).fill(0);
    for (let j = 0; j < b.length; j++) {
      row[j + 1] = line === b[j] ? prev[j]! + 1 : Math.max(prev[j + 1]!, row[j]!);
    }
    prev = row;
  }
  const common = prev[b.length]!;
  return { added: b.length - common, removed: a.length - common };
}

/** Why a call waited for an approval, from ognivo's refusal: the reason it asked for one,
 * or the rule that denied it. */
export function refusalReason(text: string): string {
  const asked = /needs the user's approval \(([^)]*)\)/.exec(text);
  if (asked) return asked[1]!;
  const rule = /is denied by the rule (.+?)\. Do not/.exec(text);
  return rule ? `denied by the rule ${rule[1]}` : text.replace(/^\[refused\]\s*/, "");
}
