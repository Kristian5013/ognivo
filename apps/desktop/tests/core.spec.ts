import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { type Server, createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { _electron as electron, expect, test } from "@playwright/test";

import { gpuEnv } from "../gpu.js";

// The GUI on a real core (docs/architecture.md §3): a session starts from the form, a
// turn streams, its command waits for the user's yes in the thread, and its change is
// reverted from the Changes panel; subagents show as lanes whose threads open; the
// settings change the look; the core quits once the window is gone. The model is a fake
// llama-server that streams what llama-server streams; no GPU, no network.

const main = fileURLToPath(new URL("../main.js", import.meta.url));
const ognivo = fileURLToPath(new URL("../../../target/debug/ognivo", import.meta.url));

const chunk = (delta: object, finish: string | null = null) => ({
  choices: [{ index: 0, delta, finish_reason: finish }],
});
const usage = { choices: [], usage: { prompt_tokens: 300, completion_tokens: 20 }, timings: { cache_n: 280, prompt_n: 20, predicted_n: 20 } };

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

/** A llama-server of one slot that answers the chat requests with `replies`, in order. */
function fakeServer(replies: object[][]): Promise<{ server: Server; url: string }> {
  const server = createServer((req, res) => {
    if (req.method === "GET" && req.url?.startsWith("/props")) {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ default_generation_settings: { n_ctx: 4096 }, total_slots: 1 }));
      return;
    }
    if (req.method !== "POST" || req.url !== "/v1/chat/completions") {
      res.statusCode = 404;
      res.end();
      return;
    }
    req.resume();
    req.on("end", () => {
      const events = replies.shift() ?? answer("Nothing more.");
      res.setHeader("content-type", "text/event-stream");
      res.end(`${events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join("")}data: [DONE]\n\n`);
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

/** The window on a core of its own, with a fake llama-server answering `replies`. */
async function launch(replies: object[][]) {
  const dir = mkdtempSync(join(tmpdir(), "ognivo-e2e-"));
  const project = join(dir, "proj");
  mkdirSync(project);
  writeFileSync(join(project, "README.md"), "a project\n");
  // a project with a build: an unverified answer is sent back once to run it
  writeFileSync(join(project, "Makefile"), "all:\n");
  const { server, url } = await fakeServer(replies);
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined) env[k] = v;
  delete env.ELECTRON_RUN_AS_NODE;
  Object.assign(env, gpuEnv(), {
    // the user's own instruction files and sessions stay out of it
    HOME: dir,
    OGNIVO_BIN: ognivo,
    OGNIVO_SOCKET: join(dir, "run/core.sock"),
    OGNIVO_DATA_DIR: join(dir, "data"),
    OGNIVO_CORE_IDLE: "1",
    OGNIVO_INACTIVE: "1",
  });
  const app = await electron.launch({ args: [main], env });
  const page = await app.firstWindow();
  await page.getByRole("heading", { name: "New session" }).waitFor({ timeout: 30_000 });
  await page.locator("#project").fill(project);
  await page.locator("#server").fill(url);
  await page.locator("#profile").selectOption("gpt-oss-20b");
  const close = async () => {
    await app.close();
    server.close();
    // nobody there: the core quits, and its socket goes with it
    await expect.poll(() => existsSync(join(dir, "run/core.sock")), { timeout: 20_000 }).toBe(false);
  };
  return { page, project, close };
}

test("a session on the core: a turn, a question in the thread, a revert, the settings", async () => {
  const { page, project, close } = await launch([
    call("c1", "write", { path: "new.txt", content: "fresh\n" }),
    call("c2", "shell", { command: "touch made.txt" }),
    answer("Done: new.txt and made.txt are there."),
    answer("Done: new.txt and made.txt are there."),
  ]);
  try {
    // the form offers what each model has: Gemma 4 has no Native path yet, gpt-oss has one
    const native = page.getByRole("checkbox", { name: /Native path/ });
    await page.locator("#profile").selectOption("gemma-4-26b-a4b");
    await expect(native).toBeDisabled();
    await expect(page.getByText("not for gemma-4-26b-a4b yet")).toBeVisible();
    await page.locator("#profile").selectOption("gpt-oss-20b");
    await expect(native).toBeEnabled();
    // the fake server's window of 4096 is too small to compact in: the profile's context
    await expect(page.getByText("96K of working context: the conversation compacts near 68.8k")).toBeVisible();
    await page.getByRole("button", { name: "Start" }).click();
    const composer = page.locator("textarea");
    await expect(composer).toHaveAttribute("placeholder", /Enter sends/, { timeout: 30_000 });
    await expect(page.getByText("not a clean git tree")).toBeVisible();
    await expect(page.getByText("context 96K (the profile's: the server's window of 4K leaves too little to compact in), compaction near 68.8K")).toBeVisible();
    await expect(page.getByText("compacts near 68.8k")).toBeVisible();
    await composer.fill("make a file");
    await composer.press("Enter");

    // the command waits for the user, in the thread
    const allow = page.getByRole("button", { name: "Allow", exact: true });
    await allow.waitFor({ timeout: 30_000 });
    await expect(page.getByText("touch made.txt").first()).toBeVisible();
    await allow.click();
    // the answer, and again after ognivo told the model its command was not verified
    await expect(page.getByText("Done: new.txt and made.txt are there.")).toHaveCount(2, { timeout: 30_000 });
    await expect(page.getByText("Turn 1 answered")).toBeVisible();
    expect(existsSync(join(project, "made.txt"))).toBe(true);
    expect(existsSync(join(project, "new.txt"))).toBe(true);

    // the sidebar names the session by its message
    await expect(page.locator("nav").getByText("make a file")).toBeVisible();

    await page.getByRole("button", { name: "Changes" }).first().click();
    const revert = page.getByRole("button", { name: "Revert new.txt" });
    await revert.waitFor();
    await page.screenshot({ path: "test-results/core-session.png" });
    await revert.click();
    await expect.poll(() => existsSync(join(project, "new.txt"))).toBe(false);
    await expect(page.getByText(/ran touch made.txt: its side effects are not in the journal/)).toBeVisible();

    // the look: the theme and the accent change at once, and stay
    await page.getByRole("button", { name: "Settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Settings" });
    await dialog.getByRole("radio", { name: "Light" }).click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    await dialog.getByRole("radio", { name: "Violet" }).click();
    const spark = () => page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--spark").trim());
    await expect.poll(spark).toBe("#8250df");
    await page.screenshot({ path: "test-results/core-settings.png" });
    await dialog.getByRole("radio", { name: "Dark" }).click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await dialog.getByRole("button", { name: "Done" }).click();
    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark", { timeout: 30_000 });
    await expect.poll(spark).toBe("#a371f7");
  } finally {
    await close();
  }
});

test("subagents show as lanes, and their threads open", async () => {
  const tasks = [
    { title: "Count the files", goal: "How many files are there?", mode: "read" },
    { title: "Read the README", goal: "What does it say?", mode: "read" },
  ];
  const { page, close } = await launch([
    call("d1", "delegate", { tasks }),
    answer("My part is done."),
    answer("My part is done."),
    answer("Both parts are done."),
  ]);
  try {
    await page.getByText("Subagents:").click();
    await page.getByRole("button", { name: "Start" }).click();
    const composer = page.locator("textarea");
    await expect(composer).toHaveAttribute("placeholder", /Enter sends/, { timeout: 30_000 });
    await composer.fill("Look at two things, with subagents.");
    await composer.press("Enter");
    await expect(page.getByText("Both parts are done.")).toBeVisible({ timeout: 60_000 });
    const lanes = page.locator(".lane");
    await expect(lanes).toHaveCount(2);
    for (const t of tasks) await expect(lanes.filter({ hasText: t.title }).getByText("done")).toBeVisible();
    await page.screenshot({ path: "test-results/core-subagents.png" });

    await lanes.filter({ hasText: "Read the README" }).getByRole("button", { name: "Open" }).click();
    await expect(page.getByText("A subagent's thread")).toBeVisible();
    await expect(page.getByText(/Task from the main agent: Read the README/)).toBeVisible();
    await expect(page.getByText("My part is done.")).toBeVisible();
    await page.getByRole("button", { name: "← Back to the session" }).click();
    await expect(page.getByText("Both parts are done.")).toBeVisible();
  } finally {
    await close();
  }
});

test("a message written anew, panels that resize, a session deleted", async () => {
  const { page, close } = await launch([answer("First answer."), answer("Second answer."), answer("Second, better.")]);
  try {
    await page.getByRole("button", { name: "Start" }).click();
    const composer = page.locator("textarea");
    await expect(composer).toHaveAttribute("placeholder", /Enter sends/, { timeout: 30_000 });
    for (const [text, answered] of [
      ["first", "First answer."],
      ["second", "Second answer."],
    ]) {
      await composer.fill(text!);
      await composer.press("Enter");
      await expect(page.getByText(answered!)).toBeVisible({ timeout: 30_000 });
    }

    // the second message, written anew: its turn leaves the conversation, and stays to read
    const second = page.locator(".group\\/user").filter({ has: page.getByText("second", { exact: true }) });
    await second.hover();
    await second.getByRole("button", { name: "Edit" }).click();
    const anew = page.getByRole("textbox", { name: "The message, written anew" });
    await anew.fill("second, better");
    await page.getByRole("button", { name: "Send", exact: true }).first().click();
    await expect(page.getByText("Second, better.")).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText(/Turn 2 taken back/)).toBeVisible();
    await expect(page.getByText("Second answer.")).toBeVisible();
    await page.screenshot({ path: "test-results/core-rewound.png" });

    // the list is as wide as the user drags it, and stays so
    const nav = page.locator("nav");
    const before = await nav.evaluate((el) => el.getBoundingClientRect().width);
    const edge = page.getByRole("separator").first();
    const box = (await edge.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + 200);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 120, box.y + 200, { steps: 6 });
    await page.mouse.up();
    const after = await nav.evaluate((el) => el.getBoundingClientRect().width);
    expect(Math.round(after - before)).toBe(120);
    await page.reload();
    await expect.poll(() => nav.evaluate((el) => Math.round(el.getBoundingClientRect().width)), { timeout: 30_000 }).toBe(
      Math.round(after),
    );

    // deleted, the session is gone from the list
    const session = nav.locator(".group\\/session").filter({ hasText: "first" });
    await session.hover();
    await session.getByRole("button", { name: "Delete the session" }).click();
    await nav.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(nav.getByText("first")).toHaveCount(0);
    await expect(page.getByRole("heading", { name: "New session" })).toBeVisible();
  } finally {
    await close();
  }
});

test("the project's MCP server: trusted, its tool asks and answers", async () => {
  const { page, project, close } = await launch([
    call("m1", "mcp__repo__echo", { text: "hello from mcp" }),
    answer("The server said hello."),
  ]);
  const echo = fileURLToPath(new URL("../../../crates/ognivo-mcp/tests/fixtures/echo.py", import.meta.url));
  writeFileSync(
    join(project, ".mcp.json"),
    JSON.stringify({ mcpServers: { repo: { command: "python3", args: ["-I", echo] } } }),
  );
  try {
    await page.getByRole("button", { name: "Start" }).click();
    // its command shows before it runs
    const trust = page.getByRole("dialog", { name: "Trust the project's MCP servers" });
    await expect(trust.getByText(/python3 -I .*echo\.py/)).toBeVisible({ timeout: 30_000 });
    await trust.getByRole("button", { name: "Start them" }).click();
    const composer = page.locator("textarea");
    await expect(composer).toHaveAttribute("placeholder", /Enter sends/, { timeout: 30_000 });
    await expect(page.getByText("mcp repo: 3 tools")).toBeVisible();
    await composer.fill("Ask the server to say hello.");
    await composer.press("Enter");

    // a tool of a server no rule allows asks first
    const allow = page.getByRole("button", { name: "Allow", exact: true });
    await allow.waitFor({ timeout: 30_000 });
    await expect(page.getByText("repo · echo").first()).toBeVisible();
    await allow.click();
    await expect(page.getByText("The server said hello.")).toBeVisible({ timeout: 30_000 });
    const card = page.locator("button", { hasText: "repo · echo" }).first();
    await card.click();
    await expect(page.getByText("hello from mcp").last()).toBeVisible();
    await page.screenshot({ path: "test-results/core-mcp.png" });

    // the MCP panel: the thread's server with its tools and prompt, and the config files
    await page.getByRole("button", { name: "MCP", exact: true }).first().click();
    const panel = page.getByLabel("MCP", { exact: true });
    await expect(panel.getByText("3 tools, 1 prompt")).toBeVisible({ timeout: 10_000 });
    await panel.getByRole("button", { name: /repo/ }).first().click();
    await expect(panel.getByText("mcp__repo__echo")).toBeVisible();
    await expect(panel.getByText("/mcp__repo__review <file> [focus]")).toBeVisible();
    await expect(panel.getByText("~/proj/.mcp.json")).toBeVisible();
    // a server of the user's own, added and removed through the form
    const form = panel.getByRole("form", { name: "Add an MCP server" });
    await form.getByLabel("Name").fill("local-echo");
    await form.getByLabel("Command").fill(`python3 -I ${echo}`);
    await form.getByRole("button", { name: "Add" }).click();
    await expect(panel.getByText("local-echo", { exact: true })).toBeVisible({ timeout: 10_000 });
    await page.screenshot({ path: "test-results/core-mcp-panel.png" });
    await panel.getByRole("listitem").filter({ hasText: /^local-echo/ }).getByRole("button", { name: "Remove" }).click();
    await expect(panel.getByText("local-echo", { exact: true })).toHaveCount(0, { timeout: 10_000 });
  } finally {
    await close();
  }
});
