// The core's socket for the extension's pages (docs/architecture.md §3). Each page gets a
// connection of its own: lines from the socket go to the page as they are, and what the
// page sends goes to the socket once it is a request of the core's methods or an answer
// to one of the core's questions.
//
// The core runs on its own: when none answers at the socket, `ognivo core` starts,
// detached, and quits once no client has been there for a while (its --idle). It outlives
// VS Code's windows, and every window of the user's shares it. Its log is core.log beside
// the socket. On Windows the socket is a named pipe, which a core makes anew each time it
// starts and names in core.pipe of its data directory; its log is in that directory too.
//
//   OGNIVO_BIN=path        the ognivo binary, unless the setting ognivo.path names one
//   OGNIVO_SOCKET=path     the socket; $XDG_RUNTIME_DIR/ognivo/core.sock by default; on
//                          Windows a pipe (\\.\pipe\NAME), by default the one core.pipe names
//   OGNIVO_DATA_DIR=path   the data directory of the core; ~/.ognivo/data by default
//   OGNIVO_HOME=path       the home ognivo works in, ~ above (the core's and the extension's)
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
  readlinkSync,
  renameSync,
  rmSync,
  statSync,
} from "node:fs";
import { type Socket, createConnection } from "node:net";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

/** How long a core that this starts may take to listen. */
const START_MS = 10_000;
const WINDOWS = process.platform === "win32";
/** A program's file name: `ognivo.exe` on Windows. */
const EXE = WINDOWS ? ".exe" : "";
/** What a connection gets where no core listens. */
const NOBODY = new Set(["ENOENT", "ECONNREFUSED"]);

/** What a page may call (crates/ognivo-protocol/src/rpc.rs). */
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
  "changes/base",
  "core/quit",
]);

/** A variable of VS Code's own process, which the core and what it starts must not get:
 * `ELECTRON_RUN_AS_NODE` would turn `code` in the agent's commands into node. */
function editors(name: string): boolean {
  return /^(VSCODE_|ELECTRON_)/i.test(name) || name.toUpperCase() === "CHROME_CRASHPAD_PIPE_NAME";
}

/** The environment of a core this starts: the extension's without VS Code's own. */
export function coreEnv(env: Env = process.env): Env {
  const out: Env = {};
  for (const [k, v] of Object.entries(env)) if (v !== undefined && !editors(k)) out[k] = v;
  return out;
}

type Env = NodeJS.ProcessEnv;

/** The core's data directory: OGNIVO_DATA_DIR, or .ognivo/data in OGNIVO_HOME or the
 * user's home, as the core has it (ognivo_os::home). */
export function dataDir(env: Env = process.env): string {
  return env.OGNIVO_DATA_DIR || join(env.OGNIVO_HOME || homedir(), ".ognivo", "data");
}

/** Where the core listens: OGNIVO_SOCKET, or $XDG_RUNTIME_DIR/ognivo/core.sock; on Windows
 * the pipe that core.pipe names, `undefined` while no core has named one. */
export function socketPath(env: Env = process.env): string | undefined {
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
export function logPath(env: Env = process.env): string {
  if (WINDOWS) return join(dataDir(env), "core.log");
  const socket = socketPath(env);
  return join(socket ? dirname(socket) : dataDir(env), "core.log");
}

/** The ognivo binary: the setting's, or OGNIVO_BIN; or the one a platform's package of
 * the extension ships in bin/; or, in the repo, the newer of its release and debug
 * builds; or `ognivo` on PATH. */
export function binary(setting: string, extension: string, env: Env = process.env): string {
  if (setting) return setting;
  if (env.OGNIVO_BIN) return env.OGNIVO_BIN;
  const shipped = join(extension, "bin", `ognivo${EXE}`);
  if (existsSync(shipped)) return outsideExtension(shipped, env);
  // apps/vscode of the repo
  const repo = join(extension, "..", "..");
  const built = ["release", "debug"].map((p) => join(repo, "target", p, `ognivo${EXE}`)).filter((p) => existsSync(p));
  built.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return built[0] ?? "ognivo";
}

/** Where the copies of the shipped binary are, which the cores this extension starts run. */
export function cacheBin(env: Env = process.env): string {
  const cache = WINDOWS
    ? join(env.LOCALAPPDATA || join(env.USERPROFILE || homedir(), "AppData", "Local"), "ognivo")
    : join(env.XDG_CACHE_HOME || join(env.HOME || homedir(), ".cache"), "ognivo");
  return join(cache, "bin");
}

/** The process ids of cores that run a copy of [`cacheBin`] other than `bin`: started by an
 * older build of the extension. */
export async function staleCores(bin: string, env: Env = process.env): Promise<number[]> {
  const dir = cacheBin(env).toLowerCase();
  const ours = (exe: string) => exe.toLowerCase().startsWith(dir) && exe.toLowerCase() !== bin.toLowerCase();
  if (WINDOWS) {
    const { execFile } = await import("node:child_process");
    const out = await new Promise<string>((resolve) =>
      execFile(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-Command",
          "Get-CimInstance Win32_Process -Filter \"Name like 'ognivo-%'\" | ForEach-Object { \"$($_.ProcessId)|$($_.ExecutablePath)\" }",
        ],
        { windowsHide: true, timeout: 15_000 },
        (_e, stdout) => resolve(String(stdout ?? "")),
      ),
    );
    return out
      .split(/\r?\n/)
      .map((l) => l.split("|"))
      .filter(([pid, exe]) => pid && exe && ours(exe))
      .map(([pid]) => Number(pid));
  }
  const pids: number[] = [];
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      if (ours(readlinkSync(`/proc/${name}/exe`))) pids.push(Number(name));
    } catch {
      // not ours to read
    }
  }
  return pids;
}

/** A copy of the shipped binary outside the extension: the core goes on after VS Code, and
 * a program that runs holds its file on Windows, so an update could not replace the
 * extension's directory. One copy for each build, in the user's cache; the other builds'
 * copies go (one a core still runs stays, as Windows keeps it). */
export function outsideExtension(path: string, env: Env = process.env): string {
  const dir = cacheBin(env);
  const { size, mtimeMs } = statSync(path);
  const name = `ognivo-${size}-${Math.trunc(mtimeMs)}${EXE}`;
  const copy = join(dir, name);
  if (existsSync(copy)) return copy;
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const part = `${copy}.${process.pid}.part`;
  copyFileSync(path, part);
  if (!WINDOWS) chmodSync(part, 0o755);
  renameSync(part, copy);
  for (const old of readdirSync(dir)) {
    if (old === name || !old.startsWith("ognivo-") || old.endsWith(".part")) continue;
    try {
      rmSync(join(dir, old), { force: true });
    } catch {
      // a core runs it
    }
  }
  return copy;
}

function attempt(path: string): Promise<Socket> {
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
function start(bin: string, socket: string | undefined, env: Env): void {
  const args = ["core"];
  if (!WINDOWS && socket) {
    mkdirSync(dirname(socket), { recursive: true, mode: 0o700 });
    args.push("--socket", socket);
  } else if (env.OGNIVO_SOCKET) {
    args.push("--socket", env.OGNIVO_SOCKET);
  }
  if (env.OGNIVO_DATA_DIR) args.push("--data-dir", env.OGNIVO_DATA_DIR);
  if (env.OGNIVO_CORE_IDLE) args.push("--idle", env.OGNIVO_CORE_IDLE);
  const logFile = logPath(env);
  mkdirSync(dirname(logFile), { recursive: true, mode: 0o700 });
  const log = openSync(logFile, "a", 0o600);
  // windowsHide: the core is a console program, and it is started without a console
  const child = spawn(bin, args, { detached: true, stdio: ["ignore", log, log], windowsHide: true, env: coreEnv(env) });
  closeSync(log);
  child.on("error", () => {});
  child.unref();
}

/** A connection to the core, which `bin` starts if none answers. */
export async function connectCore(bin: string, env: Env = process.env): Promise<Socket> {
  const path = socketPath(env);
  if (path) {
    try {
      return await attempt(path);
    } catch (e) {
      if (!NOBODY.has((e as NodeJS.ErrnoException).code ?? "")) throw e;
    }
  }
  start(bin, path, env);
  const deadline = Date.now() + START_MS;
  for (;;) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    // on Windows the core names its pipe once it listens
    const now = socketPath(env);
    try {
      if (now) return await attempt(now);
    } catch {
      // not yet
    }
    if (Date.now() > deadline) throw new Error(`the core did not start (${bin}): see ${logPath(env)}`);
  }
}

/** A message a page may send to the core. */
export function allowed(data: unknown): data is string {
  if (typeof data !== "string" || data.includes("\n")) return false;
  let m: unknown;
  try {
    m = JSON.parse(data);
  } catch {
    return false;
  }
  if (!m || typeof m !== "object") return false;
  const o = m as { jsonrpc?: unknown; id?: unknown; method?: unknown; result?: unknown; error?: unknown };
  if (o.jsonrpc !== "2.0" || !("id" in o)) return false;
  return typeof o.method === "string" ? METHODS.has(o.method) : "result" in o || "error" in o;
}

/** The answer to a call of a page's that does not go to the core: at once, so that the
 * call does not wait for ever. */
export function refusal(data: unknown): string | undefined {
  if (typeof data !== "string") return undefined;
  try {
    const m = JSON.parse(data) as { id?: unknown; method?: unknown } | null;
    if (m && typeof m === "object" && "id" in m && typeof m.method === "string") {
      return JSON.stringify({
        jsonrpc: "2.0",
        id: m.id,
        error: { code: -32601, message: `the extension does not let the page call ${m.method}` },
      });
    }
  } catch {
    // not JSON: nothing to answer
  }
  return undefined;
}

/** Lines of a stream, as they complete: a line may come in many chunks (a long thread's
 * events), and each chunk is searched once. */
export function lines(onLine: (line: string) => void): (chunk: string) => void {
  let parts: string[] = [];
  return (chunk) => {
    let from = 0;
    for (let at = chunk.indexOf("\n"); at >= 0; at = chunk.indexOf("\n", from)) {
      parts.push(chunk.slice(from, at));
      const line = parts.join("");
      parts = [];
      if (line) onLine(line);
      from = at + 1;
    }
    if (from < chunk.length) parts.push(chunk.slice(from));
  };
}
