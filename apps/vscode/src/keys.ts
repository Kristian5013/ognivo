// The API keys of the servers ognivo works with: llama-server started with `--api-key`
// wants one with each request but `/health` (docs/architecture.md §10). They are kept in
// VS Code's secret storage (the system's keychain), by the server's root URL as the core
// names a server: never in the settings, which are text and go with Settings Sync, nor in a
// page. A call of a page's or of the extension's that names a server (`server/detect`,
// `thread/start`, `thread/resume`) gets that server's key on its way to the core, where it
// brings none. The core keeps no key: a thread resumed gets its key from the call again.

import type { SecretStorage } from "vscode";

/** The secret that holds them: a JSON object of a server's root URL and its key. */
const SECRET = "ognivo.serverKeys";
/** How long a call waits for the keys to be read at the start, at most: a keychain that
 * does not answer (one locked, waiting for its password) leaves the calls without keys. */
const READ_MS = 5_000;

/** A server's URL as the core names the server (crates/ognivo-models/src/llama.rs, `root`):
 * no slash at the end, and no `/v1`, the OpenAI base URL of the same server. */
export function serverRoot(url: string): string {
  const u = url.trim().replace(/\/+$/, "");
  return (u.endsWith("/v1") ? u.slice(0, -3) : u).replace(/\/+$/, "");
}

/** Where a call's params name a server: `server/detect`'s are one. */
const NAMED: Record<string, (params: Record<string, unknown>) => unknown> = {
  "server/detect": (p) => p,
  "thread/start": (p) => p.server,
  "thread/resume": (p) => p.server,
};

/** `line`, a call to the core, with the key of the server it names put in where it brings
 * none and `keyOf` has one; else `line` as it is. */
export function withKey(line: string, keyOf: (url: string) => string | undefined): string {
  // most lines are of other calls, some of them large (a turn's images): those are not read
  if (!Object.keys(NAMED).some((method) => line.includes(`"${method}"`))) return line;
  let m: unknown;
  try {
    m = JSON.parse(line);
  } catch {
    return line;
  }
  if (!m || typeof m !== "object") return line;
  const { method, params } = m as { method?: unknown; params?: unknown };
  const at = typeof method === "string" ? NAMED[method] : undefined;
  if (!at || !params || typeof params !== "object") return line;
  const server = at(params as Record<string, unknown>);
  if (!server || typeof server !== "object") return line;
  const s = server as { url?: unknown; api_key?: unknown };
  if (typeof s.url !== "string" || (typeof s.api_key === "string" && s.api_key !== "")) return line;
  const key = keyOf(s.url);
  if (!key) return line;
  s.api_key = key;
  return JSON.stringify(m);
}

/** The keys, as the secret storage holds them, and what changes them. */
export class ServerKeys {
  private keys = new Map<string, string>();
  private kept = "{}";
  private read = false;
  /** Resolves once the keys are read, or once reading them has taken too long. */
  readonly ready: Promise<void>;
  private readonly listeners = new Set<() => void>();
  private readonly watching: { dispose(): unknown };

  constructor(private readonly secrets: Pick<SecretStorage, "get" | "store" | "onDidChange">) {
    this.ready = new Promise<void>((resolve) => {
      const late = setTimeout(resolve, READ_MS);
      void this.load().finally(() => {
        clearTimeout(late);
        resolve();
      });
    }).then(() => {
      this.read = true;
    });
    // another window of the user's changed them
    this.watching = secrets.onDidChange((e) => {
      if (e.key === SECRET) void this.load();
    });
  }

  dispose(): void {
    this.watching.dispose();
    this.listeners.clear();
  }

  /** Whether the keys are read: until then calls that name a server wait. */
  get loaded(): boolean {
    return this.read;
  }

  /** The key of the server at `url`, if one is kept. */
  readonly get = (url: string): string | undefined => this.keys.get(serverRoot(url));

  has(url: string): boolean {
    return this.keys.has(serverRoot(url));
  }

  /** Keeps `key` for the server at `url`, or none (`undefined` or empty). */
  async set(url: string, key: string | undefined): Promise<void> {
    await this.ready;
    const next = new Map(this.keys);
    const root = serverRoot(url);
    const k = key?.trim();
    if (k) next.set(root, k);
    else next.delete(root);
    const text = JSON.stringify(Object.fromEntries(next));
    await this.secrets.store(SECRET, text);
    this.take(next, text);
  }

  /** Hears every change of the keys, this window's and another's. */
  onDidChange(listener: () => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  private async load(): Promise<void> {
    let text: string | undefined;
    try {
      text = await this.secrets.get(SECRET);
    } catch {
      // a keychain that cannot be read: no keys
      return;
    }
    const keys = new Map<string, string>();
    try {
      const parsed: unknown = text ? JSON.parse(text) : {};
      if (parsed && typeof parsed === "object") {
        for (const [root, key] of Object.entries(parsed)) if (typeof key === "string" && key) keys.set(root, key);
      }
    } catch {
      // not ours to read: none
    }
    this.take(keys, JSON.stringify(Object.fromEntries(keys)));
  }

  private take(keys: Map<string, string>, text: string): void {
    const changed = text !== this.kept;
    this.keys = keys;
    this.kept = text;
    if (changed) for (const l of this.listeners) l();
  }
}

/** Lines to the core in the order they come, each with the key of the server it names put
 * in ([`withKey`]); those that come before the keys are read wait for them, and so do the
 * ones after them. */
export class KeyedLines {
  private behind?: Promise<void>;

  constructor(
    private readonly keys: Pick<ServerKeys, "loaded" | "ready" | "get">,
    private readonly write: (line: string) => void,
  ) {}

  send(line: string): void {
    if (this.keys.loaded && !this.behind) {
      this.write(withKey(line, this.keys.get));
      return;
    }
    const after = (this.behind ?? this.keys.ready).then(() => this.write(withKey(line, this.keys.get)));
    this.behind = after;
    void after.finally(() => {
      if (this.behind === after) this.behind = undefined;
    });
  }
}
