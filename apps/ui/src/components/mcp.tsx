// The MCP panel of a thread (docs/architecture.md §10): the servers the thread started, with
// their state, tools and prompts; sign-ins to remote servers; and the files that configure
// the project's servers, which it writes. A thread that starts after a change gets it.

import { useEffect, useState } from "react";

import type { Core } from "../core.ts";
import type * as P from "../protocol.ts";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

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

/** A server's entry as a config file holds it, from the form. */
export function entry(
  kind: "command" | "url",
  target: string,
  extra: string,
): Record<string, unknown> | string {
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

const STATE: Record<string, string> = {
  running: "text-ok",
  failed: "text-err",
  sign_in: "text-warn",
  untrusted: "text-muted-foreground",
};

function Server({
  s,
  busy,
  onSignIn,
  onSignOut,
}: {
  s: P.McpServerState;
  busy: boolean;
  onSignIn: () => void;
  onSignOut: () => void;
}) {
  const [open, setOpen] = useState(false);
  const button = "rounded border border-border px-2 py-0.5 hover:bg-raised disabled:opacity-40";
  const counted =
    s.state === "running"
      ? `${s.tools.length} tool${s.tools.length === 1 ? "" : "s"}${s.prompts.length ? `, ${s.prompts.length} prompt${s.prompts.length === 1 ? "" : "s"}` : ""}`
      : s.state === "sign_in"
        ? "wants a sign-in"
        : s.state === "untrusted"
          ? "waits for trust"
          : "did not start";
  return (
    <li className="rounded border border-border">
      <div className="flex items-center gap-2 px-2 py-1.5">
        <button type="button" onClick={() => setOpen(!open)} className="min-w-0 flex-1 text-left" aria-expanded={open}>
          <span className="font-medium text-spark">{s.name}</span>{" "}
          <span className={STATE[s.state] ?? ""}>{counted}</span>
          <span className="block truncate font-mono text-[11px] text-muted-foreground">
            {s.command} · {s.source}
          </span>
        </button>
        {s.remote && s.signed_in && (
          <button type="button" disabled={busy} onClick={onSignOut} className={button}>
            Sign out
          </button>
        )}
        {s.remote && !s.signed_in && (
          <button type="button" disabled={busy} onClick={onSignIn} className={button}>
            Sign in
          </button>
        )}
      </div>
      {open && (
        <div className="space-y-2 border-t border-border px-2 py-1.5">
          {s.why && <p className="text-muted-foreground">{s.why}</p>}
          {s.instructions && <p className="whitespace-pre-wrap">{s.instructions}</p>}
          {s.tools.length > 0 && (
            <ul className="space-y-0.5">
              {s.tools.map((t) => (
                <li key={t.name}>
                  <span className="font-mono">{t.name}</span> <span className="text-muted-foreground">{t.description}</span>
                </li>
              ))}
            </ul>
          )}
          {s.prompts.length > 0 && (
            <ul className="space-y-0.5">
              {s.prompts.map((p) => (
                <li key={p.name}>
                  <span className="font-mono">
                    /{p.name}
                    {p.arguments.map((a) => (a.required ? ` <${a.name}>` : ` [${a.name}]`)).join("")}
                  </span>{" "}
                  <span className="text-muted-foreground">{p.description}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </li>
  );
}

function AddServer({ busy, onAdd }: { busy: boolean; onAdd: (scope: string, name: string, value: Record<string, unknown>) => void }) {
  const [scope, setScope] = useState("user");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<"command" | "url">("command");
  const [target, setTarget] = useState("");
  const [extra, setExtra] = useState("");
  const [why, setWhy] = useState<string>();
  const field = "w-full rounded border border-border bg-panel px-2 py-1 outline-none focus:border-spark/60";
  return (
    <form
      className="space-y-2 rounded border border-border p-2"
      aria-label="Add an MCP server"
      onSubmit={(e) => {
        e.preventDefault();
        const value = entry(kind, target, extra);
        if (typeof value === "string") {
          setWhy(value);
          return;
        }
        setWhy(undefined);
        onAdd(scope, name.trim(), value);
        setName("");
        setTarget("");
        setExtra("");
      }}
    >
      <div className="flex gap-2">
        <input required value={name} onChange={(e) => setName(e.target.value)} placeholder="name" aria-label="Name" className={`${field} font-mono`} />
        <select value={scope} onChange={(e) => setScope(e.target.value)} aria-label="Where" className={field}>
          <option value="user">mine</option>
          <option value="project">the project's</option>
        </select>
        <select value={kind} onChange={(e) => setKind(e.target.value as "command" | "url")} aria-label="Kind" className={field}>
          <option value="command">a command</option>
          <option value="url">a URL</option>
        </select>
      </div>
      <input
        required
        value={target}
        onChange={(e) => setTarget(e.target.value)}
        placeholder={kind === "command" ? "npx -y @modelcontextprotocol/server-filesystem ." : "https://example.com/mcp"}
        aria-label={kind === "command" ? "Command" : "URL"}
        className={`${field} font-mono`}
      />
      <textarea
        value={extra}
        onChange={(e) => setExtra(e.target.value)}
        rows={2}
        placeholder={kind === "command" ? "KEY=value, a line each (environment)" : "Name: value, a line each (headers)"}
        aria-label={kind === "command" ? "Environment" : "Headers"}
        className={`${field} font-mono`}
      />
      <div className="flex items-center gap-2">
        <button type="submit" disabled={busy} className="rounded bg-spark px-3 py-1 font-medium text-background hover:opacity-90 disabled:opacity-40">
          Add
        </button>
        {why && <span className="text-err">{why}</span>}
      </div>
    </form>
  );
}

export function McpPanel({ core, thread, project }: { core: Core; thread: number; project: string }) {
  const [servers, setServers] = useState<P.McpServers>();
  const [config, setConfig] = useState<P.McpConfig>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState<string>();
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let current = true;
    core.call("mcp/servers", { thread }).then(
      (s) => current && setServers(s),
      (e: unknown) => current && setError(message(e)),
    );
    core.call("mcp/config", { project }).then(
      (c) => current && setConfig(c),
      (e: unknown) => current && setError(message(e)),
    );
    return () => {
      current = false;
    };
  }, [core, thread, project, tick]);
  const act = (what: string, done: Promise<unknown>) => {
    setBusy(what);
    setError(undefined);
    done.then(
      () => setTick((t) => t + 1),
      (e: unknown) => setError(message(e)),
    ).finally(() => setBusy(undefined));
  };
  const server = { project };
  const heading = "mb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground uppercase";
  return (
    <div className="min-h-0 flex-1 space-y-4 overflow-auto p-3 text-[12px]" aria-label="MCP">
      <section>
        <h3 className={heading}>This thread's servers</h3>
        {servers && servers.servers.length === 0 && (
          <p className="text-muted-foreground">No MCP server. Add one below: a thread that starts after it gets it.</p>
        )}
        <ul className="space-y-1.5">
          {servers?.servers.map((s) => (
            <Server
              key={s.name}
              s={s}
              busy={busy !== undefined}
              onSignIn={() => act(`sign in to ${s.name}`, core.call("mcp/login", { ...server, server: s.name }))}
              onSignOut={() => act(`sign out of ${s.name}`, core.call("mcp/logout", { ...server, server: s.name }))}
            />
          ))}
        </ul>
        {servers?.problems.map((p) => (
          <p key={p} className="mt-1 text-warn">
            {p}
          </p>
        ))}
        {busy?.startsWith("sign in") && <p className="mt-1 text-muted-foreground">Sign in in your browser: the core waits for it to come back.</p>}
      </section>
      <section>
        <h3 className={heading}>Configuration</h3>
        <ul className="space-y-2">
          {config?.files.map((f) => (
            <li key={f.path}>
              <p className="font-mono text-[11px] text-muted-foreground">
                {f.path}
                {f.scope === "claude" && " · Claude Code's, read only"}
              </p>
              {f.problem && <p className="text-err">{f.problem}</p>}
              {f.servers.length === 0 && !f.problem && <p className="text-muted-foreground">no server</p>}
              <ul>
                {f.servers.map((s) => (
                  <li key={s.name} className="flex items-center gap-2 py-0.5">
                    <span className="font-mono text-spark">{s.name}</span>
                    <span className="min-w-0 flex-1 truncate font-mono text-muted-foreground">
                      {typeof s.value.url === "string"
                        ? s.value.url
                        : [s.value.command, ...((s.value.args as string[] | undefined) ?? [])].join(" ")}
                    </span>
                    {f.scope !== "claude" && (
                      <button
                        type="button"
                        disabled={busy !== undefined}
                        onClick={() => act(`remove ${s.name}`, core.call("mcp/config/set", { project, scope: f.scope, name: s.name, value: null }))}
                        className="rounded border border-border px-2 py-0.5 hover:bg-raised disabled:opacity-40"
                      >
                        Remove
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
        <div className="mt-2">
          <AddServer busy={busy !== undefined} onAdd={(scope, name, value) => act(`add ${name}`, core.call("mcp/config/set", { project, scope, name, value }))} />
        </div>
        <p className="mt-1 text-muted-foreground">A thread that starts after a change gets it; the project's servers start once you trust them.</p>
      </section>
      {error && <p className="text-err">{error}</p>}
    </div>
  );
}
