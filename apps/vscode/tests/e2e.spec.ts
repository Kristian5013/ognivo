// The extension in a real VS Code on a real core (docs/architecture.md §3, §10): the first
// message starts a session with the settings, a turn streams, its command waits for the
// user's yes in the conversation, the change it made is in the Changes view and opens in
// the editor's diff, and a revert puts the file back. The model is a fake llama-server
// that streams what llama-server streams; no GPU, no network but VS Code's download.
//
//   npm run e2e -w apps/vscode       (after cargo build -p ognivo-cli and npm run build)

import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { type Server, createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { type ElectronApplication, type FrameLocator, type Page, _electron as electron, expect, test } from "@playwright/test";
import { downloadAndUnzipVSCode } from "@vscode/test-electron";

const windows = process.platform === "win32";
// without the separator at the end: on Windows `\"` would end the argument's quoting
const extension = resolve(fileURLToPath(new URL("..", import.meta.url)));
const ognivo = fileURLToPath(new URL(`../../../target/debug/ognivo${windows ? ".exe" : ""}`, import.meta.url));

const chunk = (delta: object, finish: string | null = null) => ({
  choices: [{ index: 0, delta, finish_reason: finish }],
});
const usage = {
  choices: [],
  usage: { prompt_tokens: 300, completion_tokens: 20 },
  timings: { cache_n: 280, prompt_n: 20, predicted_n: 20 },
};

function call(id: string, name: string, args: object): object[] {
  const text = JSON.stringify(args);
  return [
    chunk({ role: "assistant", content: null }),
    chunk({ reasoning_content: "The user wants a file." }),
    chunk({ tool_calls: [{ index: 0, id, type: "function", function: { name, arguments: text.slice(0, 1) } }] }),
    chunk({ tool_calls: [{ index: 0, function: { arguments: text.slice(1) } }] }),
    chunk({}, "tool_calls"),
    usage,
  ];
}

function answer(text: string): object[] {
  return [chunk({ role: "assistant", content: null }), chunk({ content: text }), chunk({}, "stop"), usage];
}

const sse = (events: object[]) => events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join("");

/** gpt-oss's probe (profiles/gpt-oss-20b.toml): the text that is one control token of its
 * vocabulary, read from the profile so that no control token stands in this file (§5.3). */
function probe(): { text: string; id: number } {
  const toml = readFileSync(fileURLToPath(new URL("../../../profiles/gpt-oss-20b.toml", import.meta.url)), "utf8");
  const m = /^probe = \{ text = "([^"]+)", id = (\d+) \}/m.exec(toml);
  if (!m) throw new Error("gpt-oss-20b.toml has no probe");
  return { text: m[1]!, id: Number(m[2]) };
}

/** A llama-server of one slot, of gpt-oss's vocabulary, that answers the chat requests
 * with `replies`, in order. */
function fakeServer(replies: object[][]): Promise<{ server: Server; url: string }> {
  const gptOss = probe();
  const server = createServer((req, res) => {
    if (req.method === "GET" && req.url?.startsWith("/props")) {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ default_generation_settings: { n_ctx: 4096 }, total_slots: 1, modalities: { vision: false } }));
      return;
    }
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (part: string) => (body += part));
    if (req.method === "POST" && req.url === "/tokenize") {
      // the probe of each profile: only gpt-oss's is one token of this vocabulary
      req.on("end", () => {
        const { content, parse_special } = JSON.parse(body) as { content: unknown; parse_special?: boolean };
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify({ tokens: content === gptOss.text && parse_special ? [gptOss.id] : [1, 2] }));
      });
      return;
    }
    if (req.method !== "POST" || req.url !== "/v1/chat/completions") {
      res.statusCode = 404;
      res.end();
      return;
    }
    req.on("end", () => {
      res.setHeader("content-type", "text/event-stream");
      res.end(`${sse(replies.shift() ?? answer("Nothing more."))}data: [DONE]\n\n`);
    });
  });
  return new Promise((resolve) =>
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      resolve({ server, url: `http://127.0.0.1:${port}` });
    }),
  );
}

/** VS Code with the extension of this repo, a profile and a core of its own, on a project
 * and a fake llama-server answering `replies`. */
async function launch(replies: object[][]) {
  const dir = mkdtempSync(join(tmpdir(), "ognivo-e2e-"));
  const project = join(dir, "proj");
  mkdirSync(project);
  writeFileSync(join(project, "README.md"), "a project\n");
  // a project with a build: an unverified answer is sent back once to run it
  writeFileSync(join(project, "Makefile"), "all:\n");
  const { server, url } = await fakeServer(replies);
  const user = join(dir, "vscode");
  mkdirSync(join(user, "User"), { recursive: true });
  writeFileSync(
    join(user, "User", "settings.json"),
    JSON.stringify({
      "ognivo.server": url,
      "ognivo.profile": "gpt-oss-20b",
      "workbench.startupEditor": "none",
      "security.workspace.trust.enabled": false,
      "extensions.autoCheckUpdates": false,
      "update.mode": "none",
      "telemetry.telemetryLevel": "off",
      "chat.disableAIFeatures": true,
      // dialogs in the workbench, where the test sees them, not the system's
      "window.dialogStyle": "custom",
    }),
  );
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) {
    // a VS Code this runs in (a terminal of one, an extension's) must not take the window,
    // nor its crash handler
    if (v === undefined || k.startsWith("VSCODE_") || k.startsWith("ELECTRON_") || k === "CHROME_CRASHPAD_PIPE_NAME") continue;
    env[k] = v;
  }
  const socket = windows ? join(dir, "data", "core.pipe") : join(dir, "run", "core.sock");
  Object.assign(env, {
    OGNIVO_HOME: dir,
    OGNIVO_BIN: ognivo,
    OGNIVO_DATA_DIR: join(dir, "data"),
    OGNIVO_CORE_IDLE: "1",
  });
  if (!windows) Object.assign(env, { OGNIVO_SOCKET: socket });
  const code = await downloadAndUnzipVSCode(process.env.OGNIVO_VSCODE ?? "stable");
  const app = await electron.launch({
    executablePath: code,
    args: [
      project,
      `--extensionDevelopmentPath=${extension}`,
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
  const page = await app.firstWindow();
  await page.locator(".monaco-workbench").waitFor({ timeout: 60_000 });
  const close = async () => {
    await app.close();
    server.close();
    // nobody there: the core quits, and its socket (on Windows the name of its pipe) goes
    await expect.poll(() => existsSync(socket), { timeout: 20_000 }).toBe(false);
  };
  return { app, page, project, dir, close };
}

/** Runs a command of VS Code's by its title, from the command palette. */
async function command(page: Page, title: string): Promise<void> {
  await page.keyboard.press("F1");
  const input = page.locator(".quick-input-widget input");
  await input.fill(`>${title}`);
  await page.locator(".quick-input-list .monaco-list-row").filter({ hasText: title }).first().click();
}

/** The agent's page: the webview of the side bar's view. */
async function agent(page: Page): Promise<FrameLocator> {
  await command(page, "ognivo: Focus the Agent");
  const outer = page.frameLocator("iframe.webview.ready");
  const frame = outer.frameLocator("iframe#active-frame");
  await frame.locator("textarea").waitFor({ timeout: 60_000 });
  return frame;
}

async function shot(app: ElectronApplication, page: Page, name: string): Promise<void> {
  void app;
  await page.screenshot({ path: join("test-results", `${name}.png`) });
}

test("a session from the first message: a call that asks in the conversation, the change in the editor's diff, a revert", async () => {
  const { app, page, project, close } = await launch([
    call("c1", "write", { path: "new.txt", content: "fresh\n" }),
    call("c2", "shell", { command: "touch made.txt" }),
    answer("Done: new.txt and made.txt are there."),
    answer("Done: new.txt and made.txt are there."),
  ]);
  try {
    const panel = await agent(page);
    // before a session: what a new one works with
    await expect(panel.getByText("A coding agent on your own llama-server")).toBeVisible();
    await expect(panel.getByText("profile gpt-oss-20b")).toBeVisible({ timeout: 30_000 });
    await shot(app, page, "e2e-welcome");

    const message = panel.getByRole("textbox", { name: "Message" });
    await message.fill("make a file");
    await message.press("Enter");

    // the command waits for the user, in the conversation
    const allow = panel.getByRole("button", { name: "Allow", exact: true });
    await allow.waitFor({ timeout: 30_000 });
    await expect(panel.getByText("touch made.txt").first()).toBeVisible();
    await shot(app, page, "e2e-approval");
    await allow.click();
    await expect(panel.getByText("Done: new.txt and made.txt are there.").first()).toBeVisible({ timeout: 30_000 });
    expect(existsSync(join(project, "made.txt"))).toBe(true);
    expect(existsSync(join(project, "new.txt"))).toBe(true);

    // the view's title names the session by its message
    await expect(page.locator(".pane-header").filter({ hasText: "make a file" })).toBeVisible({ timeout: 30_000 });

    // the change, in the Changes view and in the editor's diff
    const file = page.getByRole("treeitem", { name: /new\.txt/ });
    await file.waitFor({ timeout: 30_000 });
    await file.click();
    await expect(page.locator(".tab").filter({ hasText: "new.txt (before ognivo ↔ now)" })).toBeVisible({ timeout: 30_000 });
    await shot(app, page, "e2e-diff");

    // the conversation follows its end: the answer is in sight
    await expect(panel.getByText("Done: new.txt and made.txt are there.").last()).toBeInViewport();

    // put back: the agent created it, so it goes
    const row = page.locator(".monaco-list-row").filter({ hasText: "new.txt" });
    await row.hover();
    await row.locator(".action-label[aria-label='Revert']").click();
    await page.locator(".monaco-dialog-box").getByRole("button", { name: "Revert", exact: true }).click();
    await expect.poll(() => existsSync(join(project, "new.txt")), { timeout: 30_000 }).toBe(false);
    // the file is gone from the view; what is left is the command's side effects
    await expect(row).toHaveCount(0);
    await expect(page.getByText("and 1 command with side effects")).toBeVisible();
    await shot(app, page, "e2e-reverted");
  } finally {
    await close();
  }
});

test("the settings: Exa switched on with a key and off again, the user's instructions saved", async () => {
  const { app, page, dir, close } = await launch([]);
  try {
    const panel = await agent(page);
    await command(page, "ognivo: MCP Servers");
    const exa = panel.getByRole("switch", { name: "Switch Exa on" });
    await exa.waitFor({ timeout: 30_000 });
    await exa.click();
    await panel.getByRole("textbox", { name: "Exa API key" }).fill("test-key");
    await panel.getByRole("button", { name: "Save and switch on" }).click();
    await expect(panel.getByRole("switch", { name: "Switch Exa off" })).toBeVisible({ timeout: 30_000 });
    const config = join(dir, ".ognivo", "mcp.json");
    const exaEntry = () => (JSON.parse(readFileSync(config, "utf8")) as { mcpServers: Record<string, Record<string, unknown>> }).mcpServers.exa;
    expect(exaEntry()).toEqual({ command: "npx", args: ["-y", "exa-mcp-server"], env: { EXA_API_KEY: "test-key" } });
    await shot(app, page, "e2e-mcp");
    // off: the entry stays, with its key, until it is on again
    await panel.getByRole("switch", { name: "Switch Exa off" }).click();
    await expect(panel.getByRole("switch", { name: "Switch Exa on" })).toBeVisible({ timeout: 30_000 });
    expect(exaEntry()).toMatchObject({ disabled: true, env: { EXA_API_KEY: "test-key" } });

    await panel.getByRole("tab", { name: "Instructions" }).click();
    const mine = panel.locator("textarea.instructions");
    await mine.fill("Answer in Russian.");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(panel.getByText("saved", { exact: true })).toBeVisible();
    expect(readFileSync(join(dir, ".ognivo", "AGENTS.md"), "utf8")).toBe("Answer in Russian.");
    await shot(app, page, "e2e-instructions");
  } finally {
    await close();
  }
});
