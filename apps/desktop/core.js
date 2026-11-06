// The core's socket for the page (docs/architecture.md §3). A page that asks gets a
// MessagePort, and each message on it is one JSON-RPC message of the core's: lines from
// the socket go to the page as they are, and what the page sends goes to the socket once
// it is a request of the core's methods or an answer to one of the core's questions.
// Main has its word too: `{ core: "closed" | "failed", why? }`.
//
// The core runs on its own: when none answers at the socket, `ognivo core` starts,
// detached, and quits once no client has been there for a while (its --idle). Its log is
// core.log beside the socket.
//
//   OGNIVO_BIN=path        the ognivo binary; else the repo's newest build, else PATH's
//   OGNIVO_SOCKET=path     the socket; $XDG_RUNTIME_DIR/ognivo/core.sock by default
//   OGNIVO_DATA_DIR=path   the data directory of a core this starts
//   OGNIVO_CORE_IDLE=s     how long a core this starts stays without a client (its --idle)

import { spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, statSync } from "node:fs";
import { createConnection } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
/** How long a core that this starts may take to listen. */
const START_MS = 10_000;

/** What the page may call (crates/ognivo-protocol/src/rpc.rs). */
const METHODS = new Set([
  "initialize",
  "thread/list",
  "server/detect",
  "mcp/servers",
  "mcp/config",
  "mcp/config/set",
  "mcp/login",
  "mcp/logout",
  "thread/start",
  "thread/resume",
  "thread/read",
  "thread/trajectory",
  "thread/clear",
  "thread/close",
  "thread/delete",
  "turn/start",
  "turn/interrupt",
  "turn/answer_now",
  "turn/edit",
  "changes/list",
  "changes/accept",
  "changes/revert",
]);

export function socketPath() {
  if (process.env.OGNIVO_SOCKET) return process.env.OGNIVO_SOCKET;
  const dir = process.env.XDG_RUNTIME_DIR;
  if (!dir) throw new Error("XDG_RUNTIME_DIR is not set; set OGNIVO_SOCKET");
  return join(dir, "ognivo/core.sock");
}

/** OGNIVO_BIN, or the newer of the repo's release and debug builds, or `ognivo`. */
function binary() {
  if (process.env.OGNIVO_BIN) return process.env.OGNIVO_BIN;
  const built = ["release", "debug"].map((p) => join(REPO, "target", p, "ognivo")).filter((p) => existsSync(p));
  built.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return built[0] ?? "ognivo";
}

function attempt(path) {
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    socket.once("connect", () => {
      socket.removeAllListeners("error");
      resolve(socket);
    });
    socket.once("error", reject);
  });
}

/** Starts `ognivo core` on `socket`, on its own. */
function start(socket) {
  const dir = dirname(socket);
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const args = ["core", "--socket", socket];
  if (process.env.OGNIVO_DATA_DIR) args.push("--data-dir", process.env.OGNIVO_DATA_DIR);
  if (process.env.OGNIVO_CORE_IDLE) args.push("--idle", process.env.OGNIVO_CORE_IDLE);
  const log = openSync(join(dir, "core.log"), "a", 0o600);
  const child = spawn(binary(), args, { detached: true, stdio: ["ignore", log, log] });
  closeSync(log);
  child.on("error", () => {});
  child.unref();
}

/** A connection to the core, which starts if none answers. */
export async function connectCore() {
  const path = socketPath();
  try {
    return await attempt(path);
  } catch (e) {
    if (e.code !== "ENOENT" && e.code !== "ECONNREFUSED") throw e;
  }
  start(path);
  const deadline = Date.now() + START_MS;
  for (;;) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    try {
      return await attempt(path);
    } catch {
      if (Date.now() > deadline) {
        throw new Error(`the core did not start: see ${join(dirname(path), "core.log")}`);
      }
    }
  }
}

/** A message the page may send to the core. */
function allowed(data) {
  if (typeof data !== "string" || data.includes("\n")) return false;
  let m;
  try {
    m = JSON.parse(data);
  } catch {
    return false;
  }
  if (!m || typeof m !== "object" || m.jsonrpc !== "2.0" || !("id" in m)) return false;
  return typeof m.method === "string" ? METHODS.has(m.method) : "result" in m || "error" in m;
}

/** Joins the page's `port` to the core, starting it if it must. */
export async function bridge(port) {
  let socket;
  try {
    socket = await connectCore();
  } catch (e) {
    port.postMessage({ core: "failed", why: e instanceof Error ? e.message : String(e) });
    port.close();
    return;
  }
  // a line may come in many chunks (a long thread's events): each chunk is searched once
  let parts = [];
  socket.setEncoding("utf8");
  socket.on("data", (chunk) => {
    let start = 0;
    for (let at = chunk.indexOf("\n"); at >= 0; at = chunk.indexOf("\n", start)) {
      parts.push(chunk.slice(start, at));
      const line = parts.join("");
      parts = [];
      if (line) port.postMessage(line);
      start = at + 1;
    }
    if (start < chunk.length) parts.push(chunk.slice(start));
  });
  socket.on("error", () => {});
  socket.on("close", () => {
    port.postMessage({ core: "closed", why: "the core closed the connection" });
    port.close();
  });
  port.on("message", ({ data }) => {
    if (allowed(data)) socket.write(`${data}\n`);
  });
  port.on("close", () => socket.destroy());
  port.start();
}
