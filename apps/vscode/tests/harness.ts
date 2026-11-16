// What the end-to-end tests stand on (e2e.spec.ts): VS Code with the extension of this repo
// on a real core, a project and a profile of their own, against a fake llama-server.
//
// - The fake server streams what upstream llama-server streams with `return_progress` and
//   `timings_per_token`: how far it read the prompt, then each delta with the server's
//   count so far. A reply can drip (a pause between its chunks), wait for the test (a
//   gate), or never end. Every request is kept, so a test can read what the core sent.
// - VS Code is the one CI pins (OGNIVO_VSCODE, 1.146.0 by default), with a profile, an
//   extensions directory and a core of its own in a temp directory, which goes with it. Its
//   window shows on the desktop the tests run on, and acts as focused whatever the desktop
//   does: the tests' keys and clicks reach it directly.
// - VS Code's command palette and the agent's page are driven as a user would: the
//   palette's row runs only once the palette stays open and that row has the focus (a click
//   on a list that still filters lands on whatever row passes under it), and the page is
//   used once VS Code gave it the focus.

import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, rmSync, writeFileSync } from "node:fs";
import { type IncomingMessage, type Server, type ServerResponse, createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { type ElectronApplication, type FrameLocator, type Locator, type Page, _electron as electron, test as base, expect } from "@playwright/test";
import { downloadAndUnzipVSCode } from "@vscode/test-electron";

export { expect };

const WINDOWS = process.platform === "win32";
// without the separator at the end: on Windows `\"` would end the argument's quoting
const EXTENSION = resolve(fileURLToPath(new URL("..", import.meta.url)));
const REPO = resolve(EXTENSION, "..", "..");
const OGNIVO = join(REPO, "target", "debug", `ognivo${WINDOWS ? ".exe" : ""}`);
/** The VS Code the tests run in: CI's, unless OGNIVO_VSCODE names another. */
const VSCODE = process.env.OGNIVO_VSCODE || "1.146.0";

const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

// ---------------------------------------------------------------- what llama-server streams

const META = { id: "chatcmpl-e2e", object: "chat.completion.chunk", model: "gpt-oss-20b", system_fingerprint: "b9999-e2e00000" };

/** A chunk of a chat stream with `delta`. */
export function chunk(delta: object, finish: string | null = null): object {
  return { choices: [{ index: 0, delta, finish_reason: finish }], created: Math.floor(Date.now() / 1000), ...META };
}

/** The server's timings as `timings_per_token` puts them in each chunk: `predicted` tokens
 * so far, at `tokenMs` each, after a prompt of `prompt` tokens read and `cache` from the
 * cache. */
export function timings(predicted: number, o: { cache?: number; prompt?: number; promptMs?: number; tokenMs?: number } = {}): object {
  const cache = o.cache ?? 280;
  const prompt = o.prompt ?? 20;
  const promptMs = o.promptMs ?? 35;
  const tokenMs = o.tokenMs ?? 30;
  return {
    cache_n: cache,
    prompt_n: prompt,
    prompt_ms: promptMs,
    prompt_per_token_ms: promptMs / prompt,
    prompt_per_second: (1000 * prompt) / promptMs,
    predicted_n: predicted,
    predicted_ms: predicted * tokenMs,
    predicted_per_token_ms: predicted ? tokenMs : 0,
    predicted_per_second: predicted ? 1000 / tokenMs : 0,
  };
}

/** `chunk(delta)` with the server's count of the tokens so far. */
export function counted(delta: object, predicted: number, finish: string | null = null): object {
  return { ...chunk(delta, finish), timings: timings(predicted) };
}

/** How far the server read a prompt of `total` tokens, `cache` of them from its cache:
 * `processed` (which counts the cached ones). On a chat stream it comes with the delta of an
 * assistant message without content. */
export function promptProgress(total: number, cache: number, processed: number, timeMs = 40): object {
  return {
    ...chunk({ role: "assistant", content: null }),
    timings: timings(0, { cache, prompt: processed - cache, promptMs: timeMs }),
    prompt_progress: { total, cache, processed, time_ms: timeMs },
  };
}

/** The last chunk: the usage of the generation, and its timings. */
export function usage(completion = 20, prompt = 300): object {
  return { choices: [], created: Math.floor(Date.now() / 1000), ...META, usage: { prompt_tokens: prompt, completion_tokens: completion }, timings: timings(completion) };
}

/** A generation that calls `name` with `args`, the arguments in two parts. */
export function call(id: string, name: string, args: object, thought = "The user wants it done."): object[] {
  const text = JSON.stringify(args);
  return [
    chunk({ role: "assistant", content: null }),
    chunk({ reasoning_content: thought }),
    chunk({ tool_calls: [{ index: 0, id, type: "function", function: { name, arguments: text.slice(0, 1) } }] }),
    chunk({ tool_calls: [{ index: 0, function: { arguments: text.slice(1) } }] }),
    chunk({}, "tool_calls"),
    usage(),
  ];
}

/** A generation that answers `text`. */
export function answer(text: string): object[] {
  return [chunk({ role: "assistant", content: null }), chunk({ content: text }), chunk({}, "stop"), usage()];
}

// ---------------------------------------------------------------- the fake llama-server

/** A pause in a reply: `ms`, or until a promise settles (a gate the test opens). */
export interface Pause {
  readonly pause: number | Promise<unknown>;
}
export type Piece = object | Pause;

/** A chat reply: its chunks in order, `drip` ms apart; `hang`: the stream never ends. */
export interface Reply {
  pieces: Piece[];
  drip?: number;
  hang?: boolean;
}

export const wait = (ms: number): Pause => ({ pause: ms });
export const until = (gate: Gate | Promise<unknown>): Pause => ({ pause: gate instanceof Gate ? gate.opened : gate });

/** `pieces`, a chunk each `ms`. */
export function drip(ms: number, ...pieces: Piece[][]): Reply {
  return { pieces: pieces.flat(), drip: ms };
}

/** A reply that never comes: the connection stays open until the client or the test closes it. */
export const HANG: Reply = { pieces: [chunk({ role: "assistant", content: null })], hang: true };

/** A point a reply waits at until the test opens it. */
export class Gate {
  open!: () => void;
  readonly opened = new Promise<void>((done) => {
    this.open = done;
  });
}

function isPause(p: Piece): p is Pause {
  return typeof p === "object" && p !== null && "pause" in p && Object.keys(p).length === 1;
}

/** A request the server got. */
export interface Received {
  method: string;
  url: string;
  /** The body, parsed when it is JSON. */
  body: unknown;
}

/** A chat request's messages, as the core sends them. */
export interface ChatBody {
  messages: { role: string; content: unknown }[];
  [key: string]: unknown;
}

/** The text of a message's content: a string, or the text parts of a list. */
export function textOf(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((p) => (p && typeof p === "object" && "text" in p ? String((p as { text: unknown }).text) : "")).join("");
  return "";
}

/** gpt-oss's probe (profiles/gpt-oss-20b.toml): the text that is one control token of its
 * vocabulary, read from the profile so that no control token stands in this file (§5.3). */
function probe(): { text: string; id: number } {
  const toml = readFileSync(join(REPO, "profiles", "gpt-oss-20b.toml"), "utf8");
  const m = /^probe = \{ text = "([^"]+)", id = (\d+) \}/m.exec(toml);
  if (!m) throw new Error("gpt-oss-20b.toml has no probe");
  return { text: m[1]!, id: Number(m[2]) };
}

/** A llama-server of one slot and `window` tokens of context, of gpt-oss's vocabulary, that
 * answers the chat requests with `replies` in order (and "Nothing more." after them). */
export class FakeServer {
  readonly requests: Received[] = [];
  url = "";
  private readonly server: Server;
  private closed = false;
  private readonly closing: Promise<void>;
  private close$!: () => void;

  private constructor(
    private readonly replies: Reply[],
    private readonly window: number,
  ) {
    this.closing = new Promise((done) => (this.close$ = done));
    const gptOss = probe();
    this.server = createServer((req, res) => {
      let raw = "";
      req.setEncoding("utf8");
      req.on("data", (part: string) => (raw += part));
      req.on("end", () => {
        let body: unknown = raw;
        try {
          body = raw ? JSON.parse(raw) : undefined;
        } catch {
          // not JSON: kept as it came
        }
        this.requests.push({ method: req.method ?? "", url: req.url ?? "", body });
        void this.answer(req, res, body, gptOss);
      });
    });
  }

  static async start(replies: (Reply | Piece[])[], window = 4096): Promise<FakeServer> {
    const s = new FakeServer(
      replies.map((r) => (Array.isArray(r) ? { pieces: r } : r)),
      window,
    );
    await new Promise<void>((done) => s.server.listen(0, "127.0.0.1", done));
    const address = s.server.address();
    s.url = `http://127.0.0.1:${typeof address === "object" && address ? address.port : 0}`;
    return s;
  }

  /** The chat requests' bodies, in order. */
  chats(): ChatBody[] {
    return this.requests.filter((r) => r.method === "POST" && r.url === "/v1/chat/completions").map((r) => r.body as ChatBody);
  }

  /** Waits for the `n`-th chat request and gives its body. */
  async chat(n: number, timeout = 60_000): Promise<ChatBody> {
    await expect.poll(() => this.chats().length, { timeout, message: `chat request ${n}` }).toBeGreaterThanOrEqual(n);
    return this.chats()[n - 1]!;
  }

  private async answer(req: IncomingMessage, res: ServerResponse, body: unknown, gptOss: { text: string; id: number }): Promise<void> {
    const json = (v: unknown) => {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(v));
    };
    if (req.method === "GET" && req.url?.startsWith("/props")) {
      return json({ default_generation_settings: { n_ctx: this.window }, total_slots: 1, modalities: { vision: false }, build_info: META.system_fingerprint });
    }
    if (req.method === "POST" && req.url === "/tokenize") {
      // the probe of each profile: only gpt-oss's is one token of this vocabulary
      const { content, parse_special } = (body ?? {}) as { content?: unknown; parse_special?: boolean };
      return json({ tokens: content === gptOss.text && parse_special ? [gptOss.id] : [1, 2] });
    }
    if (req.method !== "POST" || req.url !== "/v1/chat/completions") {
      res.statusCode = 404;
      res.end();
      return;
    }
    const reply = this.replies.shift() ?? { pieces: answer("Nothing more.") };
    let gone = false;
    res.on("close", () => (gone = true));
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    let first = true;
    for (const piece of reply.pieces) {
      if (gone || this.closed) return;
      if (isPause(piece)) {
        await Promise.race([typeof piece.pause === "number" ? sleep(piece.pause) : piece.pause, this.closing]);
        continue;
      }
      if (!first && reply.drip) await Promise.race([sleep(reply.drip), this.closing]);
      first = false;
      if (gone || this.closed) return;
      res.write(`data: ${JSON.stringify(piece)}\n\n`);
    }
    // a reply that never ends: the connection goes with the client, or with the server
    if (reply.hang) return;
    res.end("data: [DONE]\n\n");
  }

  async close(): Promise<void> {
    this.closed = true;
    this.close$();
    this.server.closeAllConnections();
    await new Promise<void>((done) => this.server.close(() => done()));
  }
}

// ---------------------------------------------------------------- VS Code

export interface LaunchOptions {
  /** The chat replies of the fake server, in order. */
  replies?: (Reply | Piece[])[];
  /** The project's files besides README.md, by their paths in it. */
  files?: Record<string, string>;
  /** User settings over the tests' own. */
  settings?: Record<string, unknown>;
  /** The server's context window (`/props` n_ctx). */
  window?: number;
}

export interface VsCode {
  app: ElectronApplication;
  page: Page;
  project: string;
  /** The temp directory: OGNIVO_HOME, VS Code's profile, the core's data. */
  dir: string;
  server: FakeServer;
  /** Closes VS Code, the server and the core, and deletes the temp directory; keeps the
   * logs and a picture in `keep` first, when it is given. */
  close(keep?: string): Promise<void>;
}

/** Variables of the process the tests run in that must not reach VS Code: a VS Code this
 * runs in (its terminal's, an extension's) would take the window, or its crash handler;
 * and the desktop entry of the user's own VS Code would file the test's window under it. */
function inherited(name: string): boolean {
  return !(
    /^(VSCODE_|ELECTRON_)/i.test(name) ||
    ["CHROME_CRASHPAD_PIPE_NAME", "CHROME_DESKTOP", "GIO_LAUNCHED_DESKTOP_FILE", "GIO_LAUNCHED_DESKTOP_FILE_PID", "BAMF_DESKTOP_FILE_HINT", "DESKTOP_STARTUP_ID", "XDG_ACTIVATION_TOKEN"].includes(
      name.toUpperCase(),
    )
  );
}

/** The agent's page in the workbench: the webview of the extension's view (a tab of the
 * agent would be a `webviewPanel`). */
export const AGENT_FRAME = 'iframe.webview.ready[src*="extensionId=ognivo.ognivo"][src*="purpose=webviewView"]';

/** Whether the agent's page has VS Code's focus: its webview is the workbench's active
 * element, so that VS Code takes the keys it passes on. */
function agentFocused(): boolean {
  const a = document.activeElement;
  return a instanceof HTMLIFrameElement && a.src.includes("extensionId=ognivo.ognivo") && a.src.includes("purpose=webviewView");
}

/** The cores this run started that are still there, by their socket: on Linux. */
function coresOn(socket: string): number[] {
  if (process.platform !== "linux") return [];
  const out: number[] = [];
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      const cmd = readFileSync(`/proc/${name}/cmdline`, "utf8").split("\0");
      if (cmd.includes(socket) && readlinkSync(`/proc/${name}/exe`).startsWith(REPO)) out.push(Number(name));
    } catch {
      // gone, or not ours to read
    }
  }
  return out;
}

/** VS Code with the extension of this repo, a profile and a core of its own, on a project
 * and a fake llama-server; back once the extension showed its panel, as it does on its
 * first start (it takes the focus then). */
export async function launch(o: LaunchOptions = {}): Promise<VsCode> {
  const dir = mkdtempSync(join(tmpdir(), "ognivo-e2e-"));
  const project = join(dir, "proj");
  for (const [path, text] of Object.entries({ "README.md": "a project\n", ...o.files })) {
    mkdirSync(dirname(join(project, path)), { recursive: true });
    writeFileSync(join(project, path), text);
  }
  const server = await FakeServer.start(o.replies ?? [], o.window);
  const user = join(dir, "vscode");
  mkdirSync(join(user, "User"), { recursive: true });
  writeFileSync(
    join(user, "User", "settings.json"),
    JSON.stringify({
      "ognivo.server": server.url,
      "ognivo.profile": "gpt-oss-20b",
      "workbench.startupEditor": "none",
      "workbench.tips.enabled": false,
      "security.workspace.trust.enabled": false,
      "extensions.autoCheckUpdates": false,
      "extensions.autoUpdate": false,
      "update.mode": "none",
      "telemetry.telemetryLevel": "off",
      "chat.disableAIFeatures": true,
      // dialogs in the workbench, where the test sees them, not the system's
      "window.dialogStyle": "custom",
      ...o.settings,
    }),
  );
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined && inherited(k)) env[k] = v;
  const socket = WINDOWS ? join(dir, "data", "core.pipe") : join(dir, "run", "core.sock");
  Object.assign(env, {
    OGNIVO_HOME: dir,
    OGNIVO_BIN: OGNIVO,
    OGNIVO_DATA_DIR: join(dir, "data"),
    OGNIVO_CORE_IDLE: "1",
    // the agent's `npm test` asks nobody about an update
    NPM_CONFIG_UPDATE_NOTIFIER: "false",
    // the commands a test types in a terminal stay out of the user's own shell history
    HISTFILE: join(dir, "shell-history"),
  });
  if (!WINDOWS) Object.assign(env, { OGNIVO_SOCKET: socket });
  const code = await downloadAndUnzipVSCode(VSCODE);
  let app: ElectronApplication;
  try {
    app = await electron.launch({
      executablePath: code,
      args: [
        project,
        `--extensionDevelopmentPath=${EXTENSION}`,
        `--user-data-dir=${user}`,
        `--extensions-dir=${join(dir, "extensions")}`,
        "--disable-workspace-trust",
        "--skip-welcome",
        "--skip-release-notes",
        "--disable-gpu",
        "--new-window",
      ],
      env,
    });
  } catch (e) {
    await server.close();
    rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    throw e;
  }
  // Playwright's handle of the process goes with the app
  const proc = app.process();
  const exited = new Promise<void>((resolve) => {
    if (proc.exitCode !== null || proc.signalCode !== null) resolve();
    else proc.once("exit", () => resolve());
  });
  let done = false;
  const close = async (keep?: string) => {
    if (done) return;
    done = true;
    const page = app.windows()[0];
    if (keep && page) await page.screenshot({ path: join(keep, "window.png"), timeout: 10_000 }).catch(() => {});
    // the trace of what the test did, with the window as it went: only of a test that failed
    await Promise.race([app.context().tracing.stop(keep ? { path: join(keep, "trace.zip") } : undefined).catch(() => {}), sleep(20_000)]);
    await Promise.race([app.close().catch(() => {}), sleep(20_000)]);
    if (proc.exitCode === null && proc.signalCode === null) proc.kill("SIGKILL");
    await Promise.race([exited, sleep(10_000)]);
    await server.close();
    // nobody there: the core quits, and its socket (on Windows the name of its pipe) goes
    const gone = await expect
      .poll(() => existsSync(socket), { timeout: 20_000 })
      .toBe(false)
      .then(
        () => true,
        () => false,
      );
    if (!gone) for (const pid of coresOn(socket)) process.kill(pid, "SIGKILL");
    if (keep) {
      for (const [from, to] of [
        [join(user, "logs"), "vscode-logs"],
        [join(dir, "run", "core.log"), "core.log"],
        [join(dir, "data", "core.log"), "core.log"],
      ] as const) {
        if (existsSync(from)) cpSync(from, join(keep, to), { recursive: true });
      }
    }
    rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
    if (!gone) throw new Error(`the core of ${socket} did not quit by itself`);
  };
  try {
    await app.context().tracing.start({ screenshots: true, snapshots: true });
    const page = await app.firstWindow();
    // room for the editor, the explorer and the agent's side bar side by side
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setBounds({ x: 0, y: 0, width: 1500, height: 940 }));
    // the window acts as focused whatever the desktop's focus does: VS Code closes its
    // command palette when the window loses the focus, and the extension's notices of a
    // window out of focus would come up
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Emulation.setFocusEmulationEnabled", { enabled: true });
    await page.locator(".monaco-workbench").waitFor({ timeout: 60_000 });
    // the extension shows its panel on its first start, and the panel takes the focus, which
    // its page loses again as it loads: a palette opened meanwhile would close under the test
    await page.frameLocator(AGENT_FRAME).frameLocator("iframe#active-frame").getByRole("textbox", { name: "Message to ognivo" }).waitFor({ timeout: 60_000 });
    // a window the desktop did not bring forward drops the first key it gets (F1 opened no
    // palette, 3 launches of 3, 2026-11-16)
    await page.bringToFront();
    await settled(page);
    trace("launch: settled");
    return { app, page, project, dir, server, close };
  } catch (e) {
    await close(test.info().outputPath());
    throw e;
  }
}

/** Says what the harness does, with E2E_TRACE=1. */
function trace(...said: unknown[]): void {
  if (process.env.E2E_TRACE) console.error(`[${new Date().toISOString().slice(11, 23)}]`, ...said);
}

/** Waits until the workbench's focus stays where it is for a second: VS Code and the
 * extension move it by themselves as they start. */
export async function settled(page: Page, ms = 1000, timeout = 20_000): Promise<void> {
  const deadline = Date.now() + timeout;
  let last = "";
  let since = Date.now();
  while (Date.now() < deadline) {
    const now = await page.evaluate(() => {
      const a = document.activeElement;
      return a ? `${a.tagName}#${a.id}.${a.className}${a instanceof HTMLIFrameElement ? a.src : ""}` : "";
    });
    if (now !== last) {
      last = now;
      since = Date.now();
    } else if (Date.now() - since >= ms) {
      return;
    }
    await sleep(100);
  }
}

/** The palette's label of a command: its title, then its key or a hint after a comma. */
function rowOf(title: string): string {
  const q = title.replace(/["\\]/g, "\\$&");
  return `.quick-input-list .monaco-list-row[aria-label="${q}"], .quick-input-list .monaco-list-row[aria-label^="${q}, "]`;
}

/** Runs a command of VS Code's by its title, from the command palette: once the palette
 * stays open, the keys take the focus to the row of exactly that title, and Enter runs it. */
export async function command(page: Page, title: string): Promise<void> {
  trace("command", title);
  const widget = page.locator(".quick-input-widget");
  const input = widget.locator(".quick-input-box input");
  const row = widget.locator(rowOf(title)).first();
  for (let attempt = 1; ; attempt++) {
    try {
      if (!(await input.isVisible())) await page.keyboard.press("F1");
      await input.waitFor({ state: "visible", timeout: 2000 });
      // the focus that moves by itself (a view that opens, the panel's first show) closes it
      await sleep(250);
      if (!(await input.isVisible())) throw new Error("the command palette closed");
      await input.fill(`>${title}`);
      await row.waitFor({ timeout: 5000 });
      // the rows of other commands that match too may come first
      for (let i = 0; !(await row.evaluate((r) => r.classList.contains("focused"))); i++) {
        if (i >= 20) throw new Error(`no key reaches the row of «${title}»`);
        await input.press("ArrowDown");
      }
      await input.press("Enter");
      // the palette closes, or the command's own picker takes its place
      await expect
        .poll(async () => (await input.isVisible()) && (await input.inputValue()) === `>${title}`, { timeout: 5000, message: "the palette took the command" })
        .toBe(false);
      return;
    } catch (e) {
      trace("command attempt", attempt, "failed:", e instanceof Error ? e.message.split("\n")[0] : e);
      if (attempt >= 5) throw new Error(`the command palette did not run «${title}»: ${e instanceof Error ? e.message : String(e)}`);
    }
  }
}

/** The agent's page, once VS Code gave it the focus: the webview of the secondary side
 * bar's view. */
export async function agent(page: Page): Promise<FrameLocator> {
  await command(page, "ognivo: Focus the Agent");
  await focused(page);
  const frame = page.frameLocator(AGENT_FRAME).frameLocator("iframe#active-frame");
  await frame.getByRole("textbox", { name: "Message to ognivo" }).waitFor({ timeout: 60_000 });
  return frame;
}

/** Waits until VS Code gave the agent's page the focus. */
export async function focused(page: Page): Promise<void> {
  await page.waitForFunction(agentFocused, undefined, { timeout: 30_000 });
}

/** The composer's text box. */
export function composer(panel: FrameLocator): Locator {
  return panel.getByRole("textbox", { name: "Message to ognivo" });
}

/** A picture of the window, in test-results/ beside the run's other output. */
export async function shot(page: Page, name: string): Promise<void> {
  await page.screenshot({ path: join("test-results", `${name}.png`) });
}

/** The tests' `vscode` launches VS Code and closes it after the test, whatever happened;
 * after a failure it keeps a picture of the window, a trace of the test, and the logs of
 * VS Code and of the core in the test's output. */
export const test = base.extend<{ vscode: (o?: LaunchOptions) => Promise<VsCode> }>({
  vscode: [
    async ({}, use, info) => {
      const launched: VsCode[] = [];
      await use(async (o) => {
        const v = await launch(o);
        launched.push(v);
        return v;
      });
      const failed = info.status !== info.expectedStatus;
      const errors: unknown[] = [];
      for (const v of launched) {
        await v.close(failed ? info.outputPath() : undefined).catch((e: unknown) => errors.push(e));
      }
      if (errors.length) throw errors[0];
    },
    { timeout: 120_000 },
  ],
});
