// A client of ognivo's core (docs/architecture.md §3): JSON-RPC 2.0 over a port, one
// JSON-RPC message as text in each message. The extension's own client has a port on
// the core's socket (src/client.ts); a page's port goes through the extension
// (src/view.ts), which adds its own word on the connection, `{ core: "closed" | "failed",
// why? }`.
//
// The core's events come to `onEvent`; its questions (approvals, trust in a project's
// rules) wait in `asks` until a client answers them or the core withdraws them.

import * as P from "./protocol.ts";

/** What the client needs of a port: a MessagePort's shape, which the page's port and the
 * extension's socket both take. */
export interface Port {
  postMessage(message: string): void;
  onmessage: ((e: { data: unknown }) => void) | null;
  start?(): void;
}

export type Status = { state: "connecting" } | { state: "ready"; core: string } | { state: "down"; why: string };

export type Ask = {
  [M in keyof P.Asks]: { id: number; method: M; params: P.Asks[M]["params"] };
}[keyof P.Asks];

export class CoreError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
  }
}

type Waiting = { resolve: (result: unknown) => void; reject: (e: Error) => void };

export class Core {
  status: Status = { state: "connecting" };
  /** The core's questions that wait for an answer, by their ids. */
  readonly asks = new Map<number, Ask>();
  private next = 1;
  private calls = new Map<number, Waiting>();
  private eventListeners = new Set<(e: P.Event) => void>();
  private listeners = new Set<() => void>();
  private version = 0;

  constructor(private readonly port: Port) {
    port.onmessage = (e) => this.receive(e.data);
    port.start?.();
  }

  /** Says who the client is; from the answer on, the stream comes. */
  async initialize(client: string): Promise<P.Initialized> {
    const init = await this.call("initialize", { client, protocol: P.VERSION });
    this.status = { state: "ready", core: init.core };
    this.changed();
    return init;
  }

  call<M extends keyof P.Methods>(method: M, params: P.Methods[M]["params"]): Promise<P.Methods[M]["result"]> {
    if (this.status.state === "down") return Promise.reject(new CoreError(0, this.status.why));
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.calls.set(id, { resolve: resolve as (r: unknown) => void, reject });
      this.port.postMessage(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  /** The user's answer to question `id`. */
  answer<M extends keyof P.Asks>(id: number, result: P.Asks[M]["result"]): void {
    if (!this.asks.delete(id)) return;
    this.port.postMessage(JSON.stringify({ jsonrpc: "2.0", id, result }));
    this.changed();
  }

  onEvent(listener: (e: P.Event) => void): () => void {
    this.eventListeners.add(listener);
    return () => this.eventListeners.delete(listener);
  }

  /** Changes with the status and the questions. */
  get stateVersion(): number {
    return this.version;
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private changed(): void {
    this.version++;
    for (const l of this.listeners) l();
  }

  private receive(data: unknown): void {
    if (typeof data !== "string") {
      const word = data as { core?: string; why?: string } | null;
      if (word?.core === "closed" || word?.core === "failed") this.down(word.why ?? "the core closed the connection");
      return;
    }
    let m: { id?: number; method?: string; params?: unknown; result?: unknown; error?: { code: number; message: string } };
    try {
      m = JSON.parse(data);
    } catch {
      return;
    }
    if (m.method === P.NOTIFY_EVENT) {
      for (const l of this.eventListeners) l(m.params as P.Event);
    } else if (m.method === P.NOTIFY_CANCEL) {
      if (this.asks.delete((m.params as P.Cancel).id)) this.changed();
    } else if (m.method !== undefined && m.id !== undefined) {
      if (
        m.method === "approval/ask" ||
        m.method === "rules/trust" ||
        m.method === "mcp/trust" ||
        m.method === "mcp/sign_in" ||
        m.method === "hooks/trust"
      ) {
        this.asks.set(m.id, { id: m.id, method: m.method, params: m.params } as Ask);
        this.changed();
      }
    } else if (m.id !== undefined) {
      const waiting = this.calls.get(m.id);
      this.calls.delete(m.id);
      if (m.error) waiting?.reject(new CoreError(m.error.code, m.error.message));
      else waiting?.resolve(m.result);
    }
  }

  private down(why: string): void {
    this.status = { state: "down", why };
    for (const w of this.calls.values()) w.reject(new CoreError(0, why));
    this.calls.clear();
    this.asks.clear();
    this.changed();
  }
}
