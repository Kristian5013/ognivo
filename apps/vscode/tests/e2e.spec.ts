// The extension in a real VS Code on a real core (docs/architecture.md §3, §10), against a
// fake llama-server (harness.ts):
// - a session from its first message: the compact approval card, the summary card at the
//   turn's end, the agent's commands in their terminal, and the review of its changes in
//   the editor (the review tab, lenses, Keep and Undo of one change), then Undo all;
// - messages written while the agent works: queued, sent into the turn, taken back when the
//   turn stops before the model read them;
// - the live line as the server reads the prompt and the model thinks and writes;
// - the ways to ask from VS Code: `@` on any file of the workspace, Ctrl+I on a selection,
//   a command that failed in the user's terminal, the Quick Fix of a problem;
// - the panel in the secondary side bar; the settings; the API key of a server started with
//   `--api-key`.
//
//   npm run e2e -w apps/vscode       (after cargo build -p ognivo-cli and npm run build)

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import type { Locator } from "@playwright/test";

import {
  AGENT_FRAME,
  Gate,
  HANG,
  agent,
  answer,
  call,
  chunk,
  command,
  composer,
  counted,
  drip,
  expect,
  focused,
  promptProgress,
  shot,
  test,
  textOf,
  until,
  usage,
  wait,
} from "./harness.ts";

/** The status bar's item of the extension. */
const STATUS = '[id="ognivo.ognivo.ognivo.status"]';

const NOTES = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet"].map((l) => `${l}\n`).join("");
/** A project whose tests pass: the agent's `npm test` verifies its change. */
const PACKAGE = `${JSON.stringify({ name: "proj", private: true, scripts: { test: "node --version" } }, null, 2)}\n`;

test("a session from the first message: the approval card, the summary card, the review in the editor", async ({ vscode }) => {
  const { page, project } = await vscode({
    files: { "notes.txt": NOTES, "package.json": PACKAGE },
    replies: [
      // one edit, two changes: lines 2 and 9
      call("c1", "edit", {
        path: "notes.txt",
        old_string: "bravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\nindia",
        new_string: "BRAVO\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\nINDIA",
      }),
      call("c2", "shell", { command: "npm test" }),
      answer("Done: BRAVO and INDIA are loud now."),
    ],
  });
  const notes = join(project, "notes.txt");
  const panel = await agent(page);
  // before a session: what a new one works with
  await expect(panel.getByRole("heading", { name: "What should we work on in proj?" })).toBeVisible();
  await expect(panel.getByText("· ready")).toBeVisible({ timeout: 30_000 });
  await shot(page, "e2e-welcome");

  await composer(panel).fill("shout two of the notes");
  await composer(panel).press("Enter");

  // the command waits for the user: one compact card above the composer
  const card = panel.getByRole("group", { name: "Run this command?" });
  await expect(card).toBeVisible({ timeout: 30_000 });
  await expect(card.locator(".ask-action")).toHaveText("$ npm test");
  await expect(card).toContainText("It runs the project's code");
  await shot(page, "e2e-approval");
  await card.getByRole("button", { name: "Run", exact: true }).click();
  // answered, it goes
  await expect.soft(card, "the approval card goes once answered").toHaveCount(0, { timeout: 10_000 });

  const said = panel.getByText("Done: BRAVO and INDIA are loud now.");
  await expect(said).toBeVisible({ timeout: 30_000 });
  expect(readFileSync(notes, "utf8")).toBe(NOTES.replace("bravo", "BRAVO").replace("india", "INDIA"));
  // the session's start notes are by the composer, not in the conversation
  await expect(panel.getByRole("button", { name: /notes? of the session$/ })).toBeVisible();

  // the turn's end: what the facts say, the file to review, what it cost
  const summary = panel.locator(".summary");
  await expect(summary).toContainText("Verified", { timeout: 30_000 });
  await expect(summary).toContainText("npm test passed after the last change");
  const file = summary.locator(".summary-file", { hasText: "notes.txt" });
  await expect(file).toContainText("+2");
  await expect(file).toContainText("−2");
  await expect(panel.locator(".turn-meta")).toContainText("gpt-oss-20b");
  // the answer stays in sight above it
  await expect(said).toBeInViewport();
  await expect(page.locator(STATUS)).toHaveAttribute("aria-label", "1 file to review");
  // the explorer marks the file
  const marked = page.locator('.explorer-folders-view .monaco-icon-label[aria-label$="notes.txt • Changed by ognivo, not reviewed"]');
  await expect(marked).toBeVisible();
  await shot(page, "e2e-summary");

  // the agent's commands, as a terminal shows them
  await command(page, "ognivo: Show the Agent's Commands");
  const terminal = page.locator(".terminal-wrapper.active .xterm-rows");
  await expect(terminal).toContainText("$ npm test", { timeout: 30_000 });
  await expect(terminal).toContainText("exit 0");
  await command(page, "View: Hide Panel");

  // every changed file in one review tab
  await summary.getByRole("button", { name: "Review all" }).click();
  await expect(page.locator(".tabs-container .tab", { hasText: "ognivo: 1 file changed" })).toBeVisible({ timeout: 30_000 });
  // what the file held before the session on the left (`ognivo-base:`), and now on the right
  const entry = page.locator(".multiDiffEditor .multiDiffEntry", { hasText: "notes.txt" });
  await expect(entry.locator(".editor.original")).toContainText("bravo", { timeout: 30_000 });
  await expect(entry.locator(".editor.original")).not.toContainText("BRAVO");
  await expect(entry.locator(".editor.modified")).toContainText("BRAVO");
  await shot(page, "e2e-review");

  // the file in the editor: the changed lines tinted, a lens above each change
  await file.click();
  const editor = page.locator('.monaco-editor[data-uri$="/notes.txt"]');
  const lenses = editor.locator(".codelens-decoration");
  await expect(lenses).toHaveCount(2, { timeout: 30_000 });
  for (const title of ["✦ ognivo", "Keep", "Undo", "Diff"]) await expect(lenses.first()).toContainText(title);
  await expect(editor.locator('.view-overlays [class*="TextEditorDecorationType"]').first()).toBeAttached();
  await shot(page, "e2e-editor");

  // Undo on the first change's lens puts its line back, and leaves the other change
  await (await lens(lenses, 0)).locator("a", { hasText: /^Undo$/ }).click();
  await expect.poll(() => readFileSync(notes, "utf8"), { timeout: 30_000 }).toBe(NOTES.replace("india", "INDIA"));
  await expect(lenses).toHaveCount(1, { timeout: 30_000 });
  await expect(file).toContainText("+1", { timeout: 30_000 });

  // Keep on the last one: nothing left to review, and the file stays as the agent left it
  await (await lens(lenses, 0)).locator("a", { hasText: /^Keep$/ }).click();
  await expect(lenses).toHaveCount(0, { timeout: 30_000 });
  await expect(summary.locator(".summary-file")).toHaveCount(0, { timeout: 30_000 });
  await expect(page.locator(STATUS)).toHaveAttribute("aria-label", "ognivo", { timeout: 30_000 });
  await expect(marked).toHaveCount(0);
  expect(readFileSync(notes, "utf8")).toBe(NOTES.replace("india", "INDIA"));
  await shot(page, "e2e-reviewed");

  // a new session's page lists this one, which deletes after VS Code's dialog
  await command(page, "ognivo: New Session");
  const recent = panel.locator(".recent li").filter({ hasText: "shout two of the notes" });
  await recent.hover();
  await recent.getByRole("button", { name: /^Delete «shout two of the notes»/ }).click();
  await page.locator(".monaco-dialog-box").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(recent).toHaveCount(0, { timeout: 30_000 });
});

/** The `n`-th of an editor's lenses from its top: VS Code keeps them in the order it made
 * them. */
async function lens(lenses: Locator, n: number): Promise<Locator> {
  const tops = await lenses.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().top));
  const order = tops.map((top, i) => ({ top, i })).sort((a, b) => a.top - b.top);
  const at = order[n];
  if (!at) throw new Error(`no lens ${n} of ${tops.length}`);
  return lenses.nth(at.i);
}

test("the settings: Exa switched on with a key and off again, the user's instructions, the mode", async ({ vscode }) => {
  const { page, dir } = await vscode();
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
  await shot(page, "e2e-mcp");
  // off: the entry stays, with its key, until it is on again
  await panel.getByRole("switch", { name: "Switch Exa off" }).click();
  await expect(panel.getByRole("switch", { name: "Switch Exa on" })).toBeVisible({ timeout: 30_000 });
  expect(exaEntry()).toMatchObject({ disabled: true, env: { EXA_API_KEY: "test-key" } });

  await panel.getByRole("tab", { name: "Instructions" }).click();
  const mine = panel.getByRole("textbox", { name: "Your instructions" });
  await mine.fill("Answer in Russian.");
  await panel.getByRole("button", { name: "Save", exact: true }).click();
  await expect(panel.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(join(dir, ".ognivo", "AGENTS.md"), "utf8")).toBe("Answer in Russian.");
  await shot(page, "e2e-instructions");

  // the mode of a new session, from the panel's own menu, into VS Code's settings
  await panel.getByRole("tab", { name: "Model" }).click();
  await panel.getByRole("button", { name: /^Mode: / }).click();
  await shot(page, "e2e-mode-menu");
  await panel.getByRole("option", { name: /^Manual/ }).click();
  await expect(panel.getByRole("button", { name: "Mode: Manual" })).toBeVisible({ timeout: 10_000 });
  await expect
    .poll(() => (JSON.parse(readFileSync(join(dir, "vscode", "User", "settings.json"), "utf8")) as Record<string, unknown>)["ognivo.mode"], { timeout: 10_000 })
    .toBe("manual");
});

test("a server started with --api-key: its key, given in the settings, goes with each request, and no setting holds it", async ({ vscode }) => {
  const { page, dir, server } = await vscode({ key: "e2e-key", replies: [answer("Done behind the key.")] });
  const panel = await agent(page);
  await command(page, "ognivo: Settings, MCP Servers and Instructions");
  await panel.getByRole("tab", { name: "Model" }).click();
  // the server answers, but not without its key: not taken for one that does not answer
  const wants = panel.getByText("The server wants an API key", { exact: true });
  await expect(wants).toBeVisible({ timeout: 30_000 });
  await panel.getByRole("textbox", { name: "The server's API key" }).fill("e2e-key");
  await panel.getByRole("button", { name: "Save", exact: true }).click();
  await expect(panel.getByRole("button", { name: "Remove", exact: true })).toBeVisible({ timeout: 30_000 });
  await expect(wants).toBeHidden({ timeout: 30_000 });
  await shot(page, "e2e-server-key");
  // VS Code's secret storage holds it, not the settings
  expect(readFileSync(join(dir, "vscode", "User", "settings.json"), "utf8")).not.toContain("e2e-key");

  await panel.getByRole("button", { name: "Back to the agent" }).click();
  await expect(panel.getByText("· ready")).toBeVisible({ timeout: 30_000 });
  await composer(panel).fill("say done");
  await composer(panel).press("Enter");
  await expect(panel.getByText("Done behind the key.")).toBeVisible({ timeout: 60_000 });
  const chats = server.requests.filter((r) => r.method === "POST" && r.url === "/v1/chat/completions");
  expect(chats.length).toBeGreaterThan(0);
  expect(chats.map((r) => r.auth)).toEqual(chats.map(() => "Bearer e2e-key"));
});

test("a message written while the agent works: queued, sent into the turn, and the next turn after it", async ({ vscode }) => {
  // the first step thinks until the test lets it go on, then reads the README
  const held = new Gate();
  const { page, server } = await vscode({
    replies: [
      drip(
        100,
        [chunk({ role: "assistant", content: null }), chunk({ reasoning_content: "Let me " }), chunk({ reasoning_content: "look at the README." }), until(held)],
        call("c1", "read", { path: "README.md" }).slice(2),
      ),
      answer("It says: a project. Hi!"),
      answer("Stopped, as you asked."),
    ],
  });
  const panel = await agent(page);
  await composer(panel).fill("what does the readme say?");
  await composer(panel).press("Enter");
  // the turn runs: the live line, and Stop in place of Send
  await expect(panel.locator(".live-line")).toBeVisible({ timeout: 30_000 });
  await expect(panel.getByRole("button", { name: "Stop" })).toBeVisible();
  await server.chat(1);

  // Enter queues while a turn runs
  await composer(panel).fill("and say hi");
  await composer(panel).press("Enter");
  await composer(panel).fill("then stop");
  await composer(panel).press("Enter");
  const queued = panel.locator(".queue .queued");
  await expect(queued).toHaveCount(2);
  await expect(queued.nth(0)).toContainText("Queued");
  await expect(queued.nth(0)).toContainText("and say hi");
  await expect(queued.nth(1)).toContainText("then stop");
  await shot(page, "e2e-queued");

  // Send now: into the running turn, for the model to read before its next step
  await queued.nth(0).getByRole("button", { name: "Send now" }).click();
  await expect(queued).toHaveCount(1);
  await expect(panel.getByRole("status").filter({ hasText: "ognivo reads it before its next step" })).toBeVisible();
  // Ctrl+Enter does the same from the composer
  await composer(panel).fill("and be brief");
  await composer(panel).press("Control+Enter");
  await expect(composer(panel)).toHaveValue("");
  await expect(queued).toHaveCount(1);
  await shot(page, "e2e-steered");

  // the step goes on; the next one's request carries what the user wrote, after the call's result
  held.open();
  const second = await server.chat(2);
  const last = second.messages.at(-1)!;
  expect(last.role).toBe("tool");
  expect(textOf(last.content)).toContain("[ognivo] The user wrote while you worked: «and say hi», then «and be brief»");
  // in the thread, where the model got them
  await expect(panel.locator(".steer", { hasText: "and say hi" })).toBeVisible({ timeout: 30_000 });
  await expect(panel.locator(".steer", { hasText: "and be brief" })).toBeVisible();
  await expect(panel.getByText("It says: a project. Hi!")).toBeVisible({ timeout: 30_000 });

  // the turn ended: what waited goes as the next turn
  const third = await server.chat(3);
  expect(third.messages.at(-1)).toMatchObject({ role: "user" });
  expect(textOf(third.messages.at(-1)!.content)).toBe("then stop");
  await expect(panel.getByText("Stopped, as you asked.")).toBeVisible({ timeout: 30_000 });
  await expect(queued).toHaveCount(0);
  await shot(page, "e2e-steer-done");
});

test("a turn stopped before the model read what was sent into it: the message waits at the queue's front", async ({ vscode }) => {
  // the server never answers the first request
  const { page, server } = await vscode({ replies: [HANG, answer("On it.")] });
  const panel = await agent(page);
  await composer(panel).fill("start a long task");
  await composer(panel).press("Enter");
  await server.chat(1);
  await expect(panel.locator(".live-line")).toContainText("Waiting for the model", { timeout: 30_000 });

  await composer(panel).fill("and also this");
  await composer(panel).press("Control+Enter");
  await expect(panel.getByRole("status").filter({ hasText: "ognivo reads it before its next step" })).toBeVisible();
  // Escape in the composer stops the turn; the model never read the message, which comes back
  await composer(panel).press("Escape");
  const queued = panel.locator(".queue .queued");
  await expect(queued).toHaveCount(1, { timeout: 30_000 });
  await expect(queued).toContainText("and also this");
  await expect(panel.getByText("Turn 1 interrupted")).toBeVisible();
  await shot(page, "e2e-undelivered");
  // a stopped turn keeps the queue: nothing goes by itself
  await page.waitForTimeout(1000);
  expect(server.chats()).toHaveLength(1);

  await queued.getByRole("button", { name: "Send now" }).click();
  const next = await server.chat(2);
  // as the next turn's message, with the core's word that the last turn was stopped
  expect(next.messages.at(-1)!.role).toBe("user");
  expect(textOf(next.messages.at(-1)!.content)).toBe("and also this\n\n[ognivo] The user stopped the last turn.");
  await expect(panel.getByText("On it.")).toBeVisible({ timeout: 30_000 });
  await expect(queued).toHaveCount(0);
});

test("Undo all asks first, then deletes the file the agent made and puts back the one it changed", async ({ vscode }) => {
  const { page, project } = await vscode({
    files: { "README.md": "a project\n" },
    replies: [
      call("c1", "write", { path: "new.txt", content: "fresh\n" }),
      call("c2", "edit", { path: "README.md", old_string: "a project", new_string: "a fine project" }),
      answer("Made new.txt and praised the project."),
    ],
  });
  const panel = await agent(page);
  await composer(panel).fill("make a file and praise the project");
  await composer(panel).press("Enter");
  await expect(panel.getByText("Made new.txt and praised the project.")).toBeVisible({ timeout: 30_000 });
  expect(existsSync(join(project, "new.txt"))).toBe(true);

  // nothing ran after the changes: the card says so, as a warning
  const summary = panel.locator(".summary");
  await expect(summary).toContainText("Not verified", { timeout: 30_000 });
  await expect(summary.locator(".summary-file")).toHaveCount(2);
  await expect(summary.locator(".summary-file", { hasText: "new.txt" })).toContainText("new");
  await shot(page, "e2e-not-verified");
  // the palette's way to the review tab
  await command(page, "ognivo: Review the Changes");
  await expect(page.locator(".tabs-container .tab", { hasText: "ognivo: 2 files changed" })).toBeVisible({ timeout: 30_000 });

  await summary.getByRole("button", { name: "Undo all" }).click();
  const dialog = page.locator(".monaco-dialog-box");
  await expect(dialog).toContainText("Undo every change of the session (2 files)?");
  await dialog.getByRole("button", { name: "Undo", exact: true }).click();
  await expect.poll(() => existsSync(join(project, "new.txt")), { timeout: 30_000 }).toBe(false);
  expect(readFileSync(join(project, "README.md"), "utf8")).toBe("a project\n");
  await expect(summary.locator(".summary-file")).toHaveCount(0, { timeout: 30_000 });
  await expect(panel.getByText("Undid the changes to 2 files")).toBeVisible();
  await expect(page.locator(STATUS)).toHaveAttribute("aria-label", "ognivo");
});

test("waiting is honest: the live line says how far the server read the prompt, then what the model writes and how fast", async ({ vscode }) => {
  const read = new Gate();
  const thought = new Gate();
  const wrote = new Gate();
  const reasoning = Array.from({ length: 12 }, (_, i) => counted({ reasoning_content: `Thinking step ${i}. ` }, i + 1));
  const words = Array.from({ length: 12 }, (_, i) => counted({ content: `word${i} ` }, 13 + i));
  const { page } = await vscode({
    replies: [
      drip(
        120,
        // a prompt of 12 400 tokens, 4 000 of them from the cache: 0%, then 60% of 8.4K
        [promptProgress(12_400, 4_000, 4_000), wait(300), promptProgress(12_400, 4_000, 9_040), until(read)],
        [chunk({ role: "assistant", content: null }), ...reasoning, until(thought)],
        [...words, until(wrote)],
        [counted({}, 24, "stop"), usage(24, 12_400)],
      ),
    ],
  });
  const panel = await agent(page);
  await composer(panel).fill("write a short poem");
  await composer(panel).press("Enter");
  const live = panel.locator(".live-line");
  const status = page.locator(STATUS);

  await expect(live).toContainText("Reading the conversation", { timeout: 30_000 });
  await expect(live).toContainText("60% of 8.4K");
  await expect(status).toHaveAttribute("aria-label", /^Reading the conversation · 60%/);
  await shot(page, "e2e-progress-reading");
  read.open();

  await expect(live).toContainText("Thinking", { timeout: 30_000 });
  await expect(live).toContainText(/\d+ tokens · [\d.]+ tok\/s/);
  await expect(status).toHaveAttribute("aria-label", /^Thinking · [\d.]+ tok\/s/);
  await shot(page, "e2e-progress-thinking");
  thought.open();

  await expect(live).toContainText("Writing the reply", { timeout: 30_000 });
  await expect(live).toContainText(/\d+ tokens · [\d.]+ tok\/s/);
  await expect(status).toHaveAttribute("aria-label", /^Writing · [\d.]+ tok\/s/);
  await shot(page, "e2e-progress-writing");
  wrote.open();

  await expect(panel.getByText(/word0 word1 .* word11/)).toBeVisible({ timeout: 30_000 });
  await expect(live).toHaveCount(0);
  await expect(status).toHaveAttribute("aria-label", "ognivo");
});

test("@ names any file of the workspace, one the session never touched", async ({ vscode }) => {
  const { page } = await vscode({
    files: { "src/deep/widget.ts": "export const widget = 1;\n" },
    replies: [call("c1", "read", { path: "README.md" }), answer("It is a project.")],
  });
  const panel = await agent(page);
  await composer(panel).fill("what is this project?");
  await composer(panel).press("Enter");
  await expect(panel.getByText("It is a project.")).toBeVisible({ timeout: 30_000 });

  // the session read README.md, and nothing under src/
  const box = composer(panel);
  await box.click();
  await box.pressSequentially("look at @wid");
  const files = panel.getByRole("listbox", { name: "Files" });
  const option = files.getByRole("option", { name: /src\/deep\/widget\.ts/ });
  await expect(option).toBeVisible({ timeout: 30_000 });
  await shot(page, "e2e-mention");
  await expect(option).toHaveAttribute("aria-selected", "true");
  await box.press("Enter");
  await expect(box).toHaveValue("look at @src/deep/widget.ts ");
  await expect(files).toHaveCount(0);
});

test("Ctrl+I on a selection: the instruction and the lines go to the agent", async ({ vscode }) => {
  const calc = "export function add(a: number, b: number) {\n  const sum = a + b;\n  return sum;\n}\n";
  const { page, server } = await vscode({
    files: { "src/calc.ts": calc },
    replies: [answer("Renamed it.")],
  });
  const panel = await agent(page);
  // the file, and its lines 2–3 selected
  await command(page, "Go to File...");
  const input = page.locator(".quick-input-widget .quick-input-box input");
  await input.fill("calc.ts");
  await page.locator(".quick-input-list .monaco-list-row.focused", { hasText: "calc.ts" }).waitFor();
  await input.press("Enter");
  const editor = page.locator('.monaco-editor[data-uri$="/src/calc.ts"]');
  await expect(editor).toBeVisible();
  await expect(editor).toHaveClass(/\bfocused\b/);
  await page.keyboard.press("Control+Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("Shift+ArrowDown");
  // the composer takes the selection as a chip of its own
  await expect(panel.locator(".ctx-chips")).toContainText("calc.ts:2–3");

  await page.keyboard.press("Control+i");
  // VS Code's comment widget on the lines, or else the input box that stands in for it
  const widget = page.locator(".review-widget", { has: page.locator('[data-uri^="comment://ognivo.prompt/"]') });
  const box = page.locator(".quick-input-widget", { hasText: "ognivo ·" });
  await expect(widget.or(box)).toBeVisible({ timeout: 10_000 });
  await shot(page, "e2e-ctrl-i");
  if (await widget.isVisible()) {
    await page.keyboard.type("rename sum to total");
    await widget.locator(".form-actions .monaco-button", { hasText: /^Edit$/ }).click();
    await expect(widget).toHaveCount(0);
  } else {
    await box.locator(".quick-input-box input").fill("rename sum to total");
    await box.locator(".quick-input-box input").press("Enter");
  }

  // Edit starts the turn: the instruction, and the lines below it as VS Code's context
  const body = await server.chat(1);
  const sent = textOf(body.messages.at(-1)!.content);
  expect(sent).toBe("rename sum to total\n\n---\nContext from VS Code:\n- Selection in `src/calc.ts`, lines 2–3:\n  ```typescript\n    const sum = a + b;\n    return sum;\n  ```");
  await expect(panel.getByText("Renamed it.")).toBeVisible({ timeout: 30_000 });
  // the message shows the words, and what it carried as a chip
  await expect(panel.locator(".msg-user-text", { hasText: "rename sum to total" })).toBeVisible();
  await expect(panel.locator(".msg-chip", { hasText: "calc.ts:2–3" })).toBeVisible();
  await shot(page, "e2e-ctrl-i-sent");
});

test("a command that failed in the user's terminal: offered by the composer, its output sent along", async ({ vscode }) => {
  const { page, server } = await vscode({
    // a shell VS Code integrates with: bash, Git Bash on Windows (as CI's agent has it)
    settings: { "terminal.integrated.defaultProfile.linux": "bash", "terminal.integrated.defaultProfile.windows": "Git Bash" },
    replies: [answer("The folder is not there.")],
  });
  const panel = await agent(page);
  await command(page, "Terminal: Create New Terminal");
  await expect(page.locator(".terminal-wrapper.active .xterm")).toHaveClass(/\bfocus\b/, { timeout: 30_000 });
  // `cat`, which no usual ~/.bashrc makes an alias: with HISTCONTROL=ignoreboth (Ubuntu's
  // ~/.bashrc), VS Code's bash integration names a command as bash runs it, so `ls` would
  // come as `ls --color=auto …`
  await page.keyboard.type("cat /nonexistent-ognivo-e2e");
  await page.keyboard.press("Enter");

  // the composer offers it, and the welcome suggests to fix it
  const offer = panel.getByRole("button", { name: "Attach cat · exit 1" });
  await expect(offer).toBeVisible({ timeout: 30_000 });
  const suggestion = panel.getByRole("button", { name: /^Fix cat \/nonexistent-ognivo-e2e/ });
  await expect(suggestion).toBeVisible();
  await expect(suggestion).toContainText("exit 1");
  await shot(page, "e2e-terminal-failure");
  await offer.click();
  await composer(panel).fill("why does it fail?");
  await composer(panel).press("Enter");

  const body = await server.chat(1);
  const sent = textOf(body.messages.at(-1)!.content);
  expect(sent).toMatch(/^why does it fail\?\n\n---\nContext from VS Code:\n/);
  // in the project, as the shell spells it (Git Bash: /c/…)
  expect(sent).toMatch(/\n- Terminal: `cat \/nonexistent-ognivo-e2e` exited with 1 in `[^`]*proj`:\n {2}```text\n/);
  expect(sent).toContain("No such file or directory");
  await expect(panel.getByText("The folder is not there.")).toBeVisible({ timeout: 30_000 });
});

test("Fix with ognivo: the Quick Fix of a problem sends it with its lines", async ({ vscode }) => {
  const { page, server } = await vscode({
    files: { "bad.json": '{\n  "a": 1\n  "b": 2\n}\n' },
    replies: [answer("A comma was missing.")],
  });
  const panel = await agent(page);
  await command(page, "Go to File...");
  const input = page.locator(".quick-input-widget .quick-input-box input");
  await input.fill("bad.json");
  await page.locator(".quick-input-list .monaco-list-row.focused", { hasText: "bad.json" }).waitFor();
  await input.press("Enter");
  await expect(page.locator('[id="status.problems"]')).toHaveAttribute("aria-label", /Errors: 1/, { timeout: 30_000 });
  // to the problem, then its Quick Fix menu
  await page.keyboard.press("F8");
  await expect(page.locator(".marker-widget")).toContainText("Expected comma");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Control+.");
  const fix = page.locator(".action-widget .monaco-list-row", { hasText: "Fix with ognivo" });
  await expect(fix).toBeVisible();
  await shot(page, "e2e-quick-fix");
  await fix.click();

  const body = await server.chat(1);
  expect(textOf(body.messages.at(-1)!.content)).toBe(
    'Fix this problem.\n\n---\nContext from VS Code:\n- Problem in `bad.json` line 3 (error, json): Expected comma\n- Selection in `bad.json`, lines 1–5:\n  ```json\n  {\n    "a": 1\n    "b": 2\n  }\n  ```',
  );
  await expect(panel.getByText("A comma was missing.")).toBeVisible({ timeout: 30_000 });
});

test("the panel is in the secondary side bar, the explorer stays on the left, the status bar brings it back", async ({ vscode }) => {
  const { page } = await vscode();
  const aux = page.locator(".part.auxiliarybar");
  await expect(aux).toBeVisible();
  await expect(aux).toHaveAttribute("data-active-composite", "workbench.view.extension.ognivo");
  await expect(aux.locator(".title-label")).toHaveText("ognivo: Agent");
  // the agent's page lies over the secondary side bar, right of the editors
  const side = (await aux.boundingBox())!;
  const frame = (await page.locator(AGENT_FRAME).boundingBox())!;
  expect(frame.x).toBeGreaterThanOrEqual(side.x - 1);
  expect(frame.x + frame.width).toBeLessThanOrEqual(side.x + side.width + 1);
  expect(side.x).toBeGreaterThan((await page.locator(".part.editor").boundingBox())!.x);
  // the explorer keeps the primary side bar; ognivo has no icon in the activity bar
  await expect(page.locator(".part.sidebar .title-label")).toHaveText("Explorer");
  await expect(page.locator('.part.activitybar [aria-label^="ognivo"]')).toHaveCount(0);
  await shot(page, "e2e-placement");

  // closed, the status bar's item opens it again, with the focus
  await command(page, "View: Hide Secondary Side Bar");
  await expect(aux).toBeHidden();
  await page.locator(STATUS).click();
  await expect(aux).toBeVisible();
  await expect(aux).toHaveAttribute("data-active-composite", "workbench.view.extension.ognivo");
  await focused(page);
});
