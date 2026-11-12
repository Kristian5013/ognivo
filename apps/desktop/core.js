// The core's socket for the page (docs/architecture.md §3). A page that asks gets a
// MessagePort, and each message on it is one JSON-RPC message of the core's: lines from
// the socket go to the page as they are, and what the page sends goes to the socket once
// it is a request of the core's methods or an answer to one of the core's questions.
// Main has its word too: `{ core: "closed" | "failed", why? }`.
//
// The core runs on its own: when none answers at the socket, `ognivo core` starts,
// detached, and quits once no client has been there for a while (its --idle). Its log is
// core.log beside the socket. On Windows the socket is a named pipe, which a core makes
// anew each time it starts and names in core.pipe of its data directory; its log is in
// that directory too.
//
//   OGNIVO_BIN=path        the ognivo binary; else the one an installed window ships, else
//                          the repo's newest build, else PATH's
//   OGNIVO_SOCKET=path     the socket; $XDG_RUNTIME_DIR/ognivo/core.sock by default; on
//                          Windows a pipe (\\.\pipe\NAME), by default the one core.pipe names
//   OGNIVO_DATA_DIR=path   the data directory of the core; ~/.ognivo/data by default
//   OGNIVO_HOME=path       the home ognivo works in, ~ above (the core's and the window's)
//   OGNIVO_CORE_IDLE=s     how long a core this starts stays without a client (its --idle)

import { spawn } from "node:child_process";
import {
  chmodSync,
  closeSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
} from "node:fs";
import { createConnection } from "node:net";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
/** How long a core that this starts may take to listen. */
const START_MS = 10_000;
const WINDOWS = process.platform === "win32";
/** A program's file name: `ognivo.exe` on Windows. */
const EXE = WINDOWS ? ".exe" : "";
/** What a connection gets where no core listens. */
const NOBODY = new Set(["ENOENT", "ECONNREFUSED"]);

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
  "thread/mode",
  "turn/start",
  "turn/interrupt",
  "turn/answer_now",
  "turn/edit",
  "changes/list",
  "changes/accept",
  "changes/revert",
]);

/** The core's data directory: OGNIVO_DATA_DIR, or .ognivo/data in OGNIVO_HOME or the
 * user's home, as the core has it (ognivo_os::home). */
export function dataDir(env = process.env) {
  return env.OGNIVO_DATA_DIR || join(env.OGNIVO_HOME || homedir(), ".ognivo", "data");
}

/** Where the core listens: OGNIVO_SOCKET, or $XDG_RUNTIME_DIR/ognivo/core.sock; on Windows
 * the pipe that core.pipe names, `undefined` while no core has named one. */
export function socketPath(env = process.env) {
  if (env.OGNIVO_SOCKET) return env.OGNIVO_SOCKET;
  if (WINDOWS) {
    let name = "";
    try {
      name = readFileSync(join(dataDir(env), "core.pipe"), "utf8").trim();
    } catch {
      // no core has started yet
    }
    return name.startsWith("\\\\.\\pipe\\") ? name : undefined;
  }
  const dir = env.XDG_RUNTIME_DIR;
  if (!dir) throw new Error("XDG_RUNTIME_DIR is not set; set OGNIVO_SOCKET");
  return join(dir, "ognivo/core.sock");
}

/** The log of a core this starts: core.log beside its socket; on Windows, where a pipe is
 * in no directory, in the data directory. */
function logPath(socket, env = process.env) {
  return join(WINDOWS ? dataDir(env) : dirname(socket), "core.log");
}

/** OGNIVO_BIN; or the binary an installed window ships beside its code (resources/ognivo
 * of the .deb and the AppImage; on Windows bin\ognivo.exe, which the installer puts on
 * PATH); or the newer of the repo's release and debug builds; or `ognivo`. */
export function binary(env = process.env, resources = process.resourcesPath) {
  if (env.OGNIVO_BIN) return env.OGNIVO_BIN;
  const shipped = resources
    ? WINDOWS
      ? join(resources, "..", "bin", "ognivo.exe")
      : join(resources, "ognivo")
    : undefined;
  if (shipped && existsSync(shipped)) return outsideImage(shipped, env);
  const built = ["release", "debug"].map((p) => join(REPO, "target", p, `ognivo${EXE}`)).filter((p) => existsSync(p));
  built.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return built[0] ?? "ognivo";
}

/** A binary of the AppImage's own, copied out of it: the image is unmounted once its
 * window quits, and the core goes on after the window. One copy for each build, in
 * ~/.cache/ognivo/bin; the other builds' copies go (a core that runs one keeps it). */
export function outsideImage(path, env = process.env) {
  const image = env.APPDIR;
  if (!env.APPIMAGE || !image || !path.startsWith(`${image}/`)) return path;
  const { size, mtimeMs } = statSync(path);
  const dir = join(env.XDG_CACHE_HOME || join(env.HOME || homedir(), ".cache"), "ognivo/bin");
  const name = `ognivo-${size}-${Math.trunc(mtimeMs)}`;
  const copy = join(dir, name);
  if (existsSync(copy)) return copy;
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const part = `${copy}.${process.pid}.part`;
  copyFileSync(path, part);
  chmodSync(part, 0o755);
  renameSync(part, copy);
  for (const old of readdirSync(dir)) {
    if (old !== name && old.startsWith("ognivo-") && !old.endsWith(".part")) rmSync(join(dir, old), { force: true });
  }
  return copy;
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

/** Starts `ognivo core` on its own: on `socket`, or on Windows without OGNIVO_SOCKET on a
 * pipe of its own choosing, which it names in core.pipe. */
function start(socket) {
  const args = ["core"];
  if (!WINDOWS) {
    mkdirSync(dirname(socket), { recursive: true, mode: 0o700 });
    args.push("--socket", socket);
  } else if (process.env.OGNIVO_SOCKET) {
    args.push("--socket", process.env.OGNIVO_SOCKET);
  }
  if (process.env.OGNIVO_DATA_DIR) args.push("--data-dir", process.env.OGNIVO_DATA_DIR);
  if (process.env.OGNIVO_CORE_IDLE) args.push("--idle", process.env.OGNIVO_CORE_IDLE);
  const logFile = logPath(socket);
  mkdirSync(dirname(logFile), { recursive: true, mode: 0o700 });
  const log = openSync(logFile, "a", 0o600);
  // windowsHide: the core is a console program, and it is started without a console
  const child = spawn(binary(), args, { detached: true, stdio: ["ignore", log, log], windowsHide: true });
  closeSync(log);
  child.on("error", () => {});
  child.unref();
}

/** A connection to the core, which starts if none answers. */
export async function connectCore() {
  const path = socketPath();
  if (path) {
    try {
      return await attempt(path);
    } catch (e) {
      if (!NOBODY.has(e.code)) throw e;
    }
  }
  start(path);
  const deadline = Date.now() + START_MS;
  for (;;) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    // on Windows the core names its pipe once it listens
    const now = socketPath();
    try {
      if (now) return await attempt(now);
    } catch {
      // not yet
    }
    if (Date.now() > deadline) {
      throw new Error(`the core did not start: see ${logPath(path ?? "")}`);
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

/** The answer to a call of the page's that does not go to the core: at once, so that the
 * call does not wait for ever. */
function refusal(data) {
  try {
    const m = JSON.parse(data);
    if (m && typeof m === "object" && "id" in m && typeof m.method === "string") {
      return JSON.stringify({
        jsonrpc: "2.0",
        id: m.id,
        error: { code: -32601, message: `the window does not let the page call ${m.method}` },
      });
    }
  } catch {
    // not JSON: nothing to answer
  }
  return undefined;
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
    if (allowed(data)) {
      socket.write(`${data}\n`);
      return;
    }
    const answer = refusal(data);
    if (answer) port.postMessage(answer);
  });
  port.on("close", () => socket.destroy());
  port.start();
}
