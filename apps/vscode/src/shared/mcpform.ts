// What "ognivo: Add MCP Server" asks, as a config file holds it (docs/architecture.md §10):
// a command line or a URL, and the server's environment or headers.

/** A command line as a shell splits it into words: quotes keep blanks together. */
export function words(line: string): string[] {
  const out: string[] = [];
  let word = "";
  let quote: string | undefined;
  let started = false;
  for (const c of line) {
    if (quote) {
      if (c === quote) quote = undefined;
      else word += c;
    } else if (c === '"' || c === "'") {
      quote = c;
      started = true;
    } else if (/\s/.test(c)) {
      if (started) out.push(word);
      word = "";
      started = false;
    } else {
      word += c;
      started = true;
    }
  }
  if (started) out.push(word);
  return out;
}

/** `KEY=value` (or `Name: value` with `sep` ":") lines as an object; blank lines are none. */
export function pairs(text: string, sep: "=" | ":"): Record<string, string> | string {
  const out: Record<string, string> = {};
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const at = line.indexOf(sep);
    if (at <= 0) return `not ${sep === "=" ? "KEY=value" : "Name: value"}: ${line}`;
    out[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  return out;
}

/** A server's entry as a config file holds it; a string says what is wrong. */
export function entry(kind: "command" | "url", target: string, extra: string): Record<string, unknown> | string {
  if (kind === "command") {
    const [command, ...args] = words(target);
    if (!command) return "a command to start the server";
    const env = pairs(extra, "=");
    if (typeof env === "string") return env;
    return Object.keys(env).length ? { command, args, env } : { command, args };
  }
  const url = target.trim();
  if (!/^https?:\/\//.test(url)) return "a URL of http:// or https://";
  const headers = pairs(extra, ":");
  if (typeof headers === "string") return headers;
  return Object.keys(headers).length ? { type: "http", url, headers } : { type: "http", url };
}
