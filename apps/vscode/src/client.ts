// The extension's own client of the core (docs/architecture.md §3), beside its pages'
// connections: the Changes view, the sessions' picker, the trust dialogs and the notices
// of questions work through it. The core asks every client and takes the first answer, so
// a question the extension answers natively is withdrawn from the pages, and the other way
// round.
//
// The core outlives VS Code, so after an update of the extension the core that runs may be
// another build than the one the extension ships. The client sees it in the core's hello
// (`exe`), asks that core to quit while no turn runs (`core/quit`), and starts its own; a
// core whose turn runs stays until the next connection finds it idle.

import type { Socket } from "node:net";
import { isAbsolute } from "node:path";

import { connectCore, lines, staleCores } from "./core.ts";
import { Core, CoreError, type Port } from "./shared/rpc.ts";
import { ERRORS } from "./shared/protocol.ts";
import { samePath } from "./shared/text.ts";

const CLIENT = "ognivo-vscode 0.0.1";

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

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

  private async open(): Promise<{ core: Core; socket: Socket; exe: string }> {
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
    const init = await core.initialize(CLIENT);
    return { core, socket, exe: init.exe };
  }

  private async connect(): Promise<Core> {
    let { core, socket, exe } = await this.open();
    const bin = this.bin();
    const gone = async () => {
      const closed = new Promise<void>((resolve) => socket.once("close", () => resolve()));
      await Promise.race([closed, sleep(5000)]);
      // the process lets go of its lock and its pipe as it ends
      await sleep(300);
      ({ core, socket, exe } = await this.open());
    };
    // another build than the one this extension starts: replaced while no turn runs
    if (isAbsolute(bin) && exe && samePath(exe) !== samePath(bin)) {
      try {
        await core.call("core/quit", {});
        await gone();
      } catch (e) {
        // a turn runs: this core stays for now
        if (!(e instanceof CoreError && e.code === ERRORS.busy)) throw e;
      }
    } else if (isAbsolute(bin) && !exe) {
      // a core from before `core/quit`, which an older build of this extension started:
      // it cannot be asked, so it ends while none of its turns runs
      const { threads } = await core.call("thread/list", { project: null, limit: 200 });
      const stale = threads.some((t) => t.running) ? [] : await staleCores(bin);
      if (stale.length) {
        for (const pid of stale) {
          try {
            process.kill(pid);
          } catch {
            // gone already
          }
        }
        await gone();
      }
    }
    this.listen(core);
    return core;
  }

  dispose(): void {
    this.socket?.destroy();
  }
}
