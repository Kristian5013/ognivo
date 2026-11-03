import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { type Server, createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { _electron as electron, expect, test } from "@playwright/test";

import { gpuEnv } from "../gpu.js";

// The GUI on a real core (docs/architecture.md §3): a session starts from the form, a
// turn streams, its command waits for the user's yes in the thread, and its change is
// reverted from the Changes panel; the core quits once the window is gone. The model is
// a fake llama-server that streams what llama-server streams; no GPU, no network.

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

test("a session on the core: a turn, a question in the thread, a revert", async () => {
  const dir = mkdtempSync(join(tmpdir(), "ognivo-e2e-"));
  const project = join(dir, "proj");
  mkdirSync(project);
  writeFileSync(join(project, "README.md"), "a project\n");
  const { server, url } = await fakeServer([
    call("c1", "write", { path: "new.txt", content: "fresh\n" }),
    call("c2", "shell", { command: "touch made.txt" }),
    answer("Done: new.txt and made.txt are there."),
    answer("Done: new.txt and made.txt are there."),
  ]);
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
  try {
    const page = await app.firstWindow();
    await page.getByRole("heading", { name: "New session" }).waitFor({ timeout: 30_000 });
    await page.locator("#project").fill(project);
    await page.locator("#server").fill(url);
    await page.locator("#profile").selectOption("gpt-oss-20b");
    await page.getByRole("button", { name: "Start" }).click();

    const composer = page.locator("textarea");
    await expect(composer).toHaveAttribute("placeholder", /Enter sends/, { timeout: 30_000 });
    await expect(page.getByText("not a clean git tree")).toBeVisible();
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
  } finally {
    await app.close();
    server.close();
  }
  // nobody there: the core quits, and its socket goes with it
  await expect.poll(() => existsSync(join(dir, "run/core.sock")), { timeout: 20_000 }).toBe(false);
});
