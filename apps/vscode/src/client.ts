// The extension's own client of the core (docs/architecture.md §3), beside its pages'
// connections: the Changes view, the sessions' picker, the trust dialogs and the notices
// of questions work through it. The core asks every client and takes the first answer, so
// a question the extension answers natively is withdrawn from the pages, and the other way
// round.

import type { Socket } from "node:net";

import { connectCore, lines } from "./core.ts";
import { Core, type Port } from "./shared/rpc.ts";

const CLIENT = "ognivo-vscode 0.0.1";

/** The client, connected on first use and again after the core went away (its --idle, an
 * update). `listen` hears each new connection's client before its first call. */
export class Connection {
  private current?: Promise<Core>;
  private socket?: Socket;

  constructor(
    private readonly bin: () => string,
    private readonly listen: (core: Core) => void,
  ) {}

  get(): Promise<Core> {
    if (!this.current) {
      const connecting = this.connect();
      this.current = connecting;
      // a failed attempt does not stay: the next call tries again
      connecting.catch(() => {
        if (this.current === connecting) this.current = undefined;
      });
    }
    return this.current;
  }

  /** The client if it is connected now, else nothing: for what may wait for the next. */
  async ready(): Promise<Core | undefined> {
    if (!this.current) return undefined;
    return this.current.then(
      (core) => (core.status.state === "ready" ? core : undefined),
      () => undefined,
    );
  }

  private async connect(): Promise<Core> {
    const socket = await connectCore(this.bin());
    this.socket = socket;
    socket.setEncoding("utf8");
    const port: Port = { onmessage: null, postMessage: (m) => socket.write(`${m}\n`) };
    socket.on(
      "data",
      lines((line) => port.onmessage?.({ data: line })),
    );
    socket.on("error", () => {});
    socket.on("close", () => {
      port.onmessage?.({ data: { core: "closed", why: "the core closed the connection" } });
      if (this.socket === socket) {
        this.socket = undefined;
        this.current = undefined;
      }
    });
    const core = new Core(port);
    this.listen(core);
    await core.initialize(CLIENT);
    return core;
  }

  dispose(): void {
    this.socket?.destroy();
  }
}
