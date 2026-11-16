// What VS Code puts on a webview's page for its theme, taken from a real VS Code: the
// --vscode-* variables on <html>, the body's classes and data attributes, and the default
// styles. The gallery puts them on its own page, so the panel there looks as in VS Code.
//
//   node gallery/capture.mjs [dark|light|hc|hc-light...]   (in apps/vscode, after npm run build)
//
// With no theme named, it takes them all.

import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { _electron as electron } from "@playwright/test";
import { downloadAndUnzipVSCode } from "@vscode/test-electron";

const windows = process.platform === "win32";
const extension = resolve(fileURLToPath(new URL("..", import.meta.url)));
const ognivo = fileURLToPath(new URL(`../../../target/debug/ognivo${windows ? ".exe" : ""}`, import.meta.url));
const out = fileURLToPath(new URL("themes/", import.meta.url));
/** Each theme by its id (the setting's value, which the page says too), and the class it
 * gives the workbench. */
const THEMES = {
  dark: { theme: "Dark Modern", workbench: "vs-dark" },
  light: { theme: "Light Modern", workbench: "vs" },
  hc: { theme: "Default High Contrast", workbench: "hc-black" },
  "hc-light": { theme: "Default High Contrast Light", workbench: "hc-light" },
};
const asked = process.argv.slice(2);
const taking = Object.entries(THEMES).filter(([name]) => asked.length === 0 || asked.includes(name));

const SETTINGS = {
  "workbench.startupEditor": "none",
  "security.workspace.trust.enabled": false,
  "extensions.autoCheckUpdates": false,
  "update.mode": "none",
  "telemetry.telemetryLevel": "off",
  "chat.disableAIFeatures": true,
  "window.autoDetectHighContrast": false,
  // nothing answers there: the page only has to be up
  "ognivo.server": "http://127.0.0.1:9",
};

mkdirSync(out, { recursive: true });
const code = await downloadAndUnzipVSCode(process.env.OGNIVO_VSCODE ?? "1.146.0");
for (const [name, { theme, workbench }] of taking) {
  const dir = mkdtempSync(join(tmpdir(), "ognivo-theme-"));
  const project = join(dir, "proj");
  const settings = join(dir, "vscode", "User", "settings.json");
  mkdirSync(project);
  mkdirSync(join(dir, "vscode", "User"), { recursive: true });
  writeFileSync(settings, JSON.stringify(SETTINGS));
  const env = {};
  for (const [k, v] of Object.entries(process.env)) {
    if (v === undefined || k.startsWith("VSCODE_") || k.startsWith("ELECTRON_") || k === "CHROME_CRASHPAD_PIPE_NAME") continue;
    env[k] = v;
  }
  // a core of its own on a socket of its own, never the user's
  Object.assign(env, {
    OGNIVO_HOME: dir,
    OGNIVO_BIN: ognivo,
    OGNIVO_DATA_DIR: join(dir, "data"),
    OGNIVO_SOCKET: windows ? `\\\\.\\pipe\\ognivo-theme-${process.pid}` : join(dir, "core.sock"),
    OGNIVO_CORE_IDLE: "1",
  });
  const app = await electron.launch({
    executablePath: code,
    args: [
      project,
      `--extensionDevelopmentPath=${extension}`,
      `--user-data-dir=${join(dir, "vscode")}`,
      `--extensions-dir=${join(dir, "extensions")}`,
      "--disable-workspace-trust",
      "--skip-welcome",
      "--skip-release-notes",
      "--disable-gpu",
      "--new-window",
    ],
    env,
  });
  try {
    const page = await app.firstWindow();
    await page.locator(".monaco-workbench").waitFor({ timeout: 60_000 });
    // The theme goes in once the window is up. At its start VS Code takes a theme from
    // under the extension's folder, where the VS Code of the tests (.vscode-test) is, over
    // the setting's; a change of the setting afterwards is taken as it says.
    writeFileSync(settings, JSON.stringify({ ...SETTINGS, "workbench.colorTheme": theme }));
    await page.locator(`.monaco-workbench.${workbench}`).waitFor({ timeout: 30_000 });
    const frame = page.frameLocator("iframe.webview.ready").frameLocator("iframe#active-frame");
    // the extension shows its panel by itself on its first start; else the command does
    const up = await frame
      .locator("#root")
      .waitFor({ timeout: 20_000 })
      .then(() => true)
      .catch(() => false);
    if (!up) {
      await page.keyboard.press("F1");
      await page.locator(".quick-input-widget input").fill(">ognivo: Focus the Agent");
      await page.locator(".quick-input-list .monaco-list-row").filter({ hasText: "Focus the Agent" }).first().waitFor({ timeout: 30_000 });
      await page.keyboard.press("Enter");
      await frame.locator("#root").waitFor({ timeout: 60_000 });
    }
    // the page has the theme's colours once it says the theme's name
    await frame.locator(`body[data-vscode-theme-id="${theme}"]`).waitFor({ timeout: 30_000 });
    const taken = await frame.locator("html").evaluate((html) => ({
      style: html.getAttribute("style") ?? "",
      bodyClass: document.body.className,
      bodyAttrs: Object.fromEntries(
        [...document.body.attributes].filter((a) => a.name.startsWith("data-")).map((a) => [a.name, a.value]),
      ),
      defaultStyles: document.getElementById("_defaultStyles")?.textContent ?? "",
    }));
    writeFileSync(join(out, `${name}.json`), `${JSON.stringify({ theme, ...taken }, null, 2)}\n`);
    console.log(`${name}: ${taken.style.split(";").length} variables, body ${taken.bodyClass}, ${taken.bodyAttrs["data-vscode-theme-name"]}`);
  } finally {
    await app.close();
  }
}
