// The panel's states as pictures: builds the gallery (the panel's page on a fake extension
// and core, fake.ts), serves it, and takes each scenario (scenarios.ts) in the system's
// Edge or Chrome, at a side bar's width.
//
//   node gallery/shoot.mjs [scenario...] [--theme=dark|light|hc|hc-light|both|all]
//                          [--width=420] [--height=860] [--scale=1] [--wait=1600]
//                          [--place=sidebar|editor] [--out=dir] [--name=suffix]
//                          [--click=selector]... [--hover=selector] [--scroll=top]
//                          [--reduce-motion] [--serve]
//
// `--click` opens what a click opens (a menu, an exploration, a command's output); given
// more than once, the clicks follow one another. `--hover` shows what the pointer shows (a
// row's buttons). `--place=editor` shows the panel as the editor tab shows it. `--name`
// goes after the scenario's name in the file's, for the pictures of a click beside the
// plain ones. `--theme=both` takes the dark and the light theme, `all` the high-contrast
// ones too (themes/, from capture.mjs). `--serve` only serves it, for a browser:
// http://127.0.0.1:<port>/?s=session&theme=light&place=editor
//
// Each run builds into a directory of its own, so that runs at the same time (two widths,
// two agents) do not take each other's build away.

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "@playwright/test";
import * as esbuild from "esbuild";

const here = fileURLToPath(new URL(".", import.meta.url));
const args = process.argv.slice(2);
/** `--name=value` and `--name`, the last one of a name; `--click`, all of them in order. */
const flags = {};
const clicks = [];
for (const a of args.filter((a) => a.startsWith("--"))) {
  // a value may hold `=` itself: `--click=button >> nth=1`
  const at = a.indexOf("=");
  const [key, value] = at < 0 ? [a.slice(2), "true"] : [a.slice(2, at), a.slice(at + 1)];
  if (key === "click") clicks.push(value);
  else flags[key] = value;
}
const named = args.filter((a) => !a.startsWith("--"));

const dist = mkdtempSync(join(tmpdir(), "ognivo-gallery-"));
const clean = () => rmSync(dist, { recursive: true, force: true });
await esbuild.build({
  entryPoints: [join(here, "main.tsx")],
  outdir: dist,
  bundle: true,
  platform: "browser",
  format: "esm",
  target: "es2022",
  jsx: "automatic",
  loader: { ".ttf": "file", ".jsonl": "text" },
  define: { "process.env.NODE_ENV": JSON.stringify("development") },
  logLevel: "warning",
});
writeFileSync(
  join(dist, "index.html"),
  `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <link rel="stylesheet" href="main.css" />
    <title>ognivo gallery</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="main.js"></script>
  </body>
</html>
`,
);

const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".ttf": "font/ttf", ".png": "image/png" };
const server = createServer((req, res) => {
  const path = new URL(req.url ?? "/", "http://x").pathname;
  try {
    const body = readFileSync(join(dist, path === "/" ? "index.html" : path));
    res.setHeader("content-type", TYPES[extname(path)] ?? "text/html");
    res.end(body);
  } catch {
    res.statusCode = 404;
    res.end();
  }
});
await new Promise((resolve) => server.listen(Number(flags.port ?? 0), "127.0.0.1", resolve));
const url = `http://127.0.0.1:${server.address().port}`;

if (flags.serve) {
  console.log(`the gallery: ${url}/?s=session`);
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => {
      clean();
      process.exit(0);
    });
  }
} else {
  const source = readFileSync(join(here, "scenarios.ts"), "utf8");
  const all = [...source.matchAll(/^ {2}"?([a-z-]+)"?: [{r]/gm)].map((m) => m[1]);
  const scenarios = named.length ? named : all;
  const captured = ["dark", "light", "hc", "hc-light"].filter((t) => existsSync(join(here, "themes", `${t}.json`)));
  const themes = flags.theme === "all" ? captured : flags.theme === "both" ? ["dark", "light"] : [flags.theme ?? "dark"];
  const place = flags.place === "editor" ? "&place=editor" : "";
  const out = flags.out ?? join(here, "shots");
  mkdirSync(out, { recursive: true });
  const browser = await chromium.launch({ channel: process.env.OGNIVO_BROWSER ?? (process.platform === "win32" ? "msedge" : "chrome") });
  try {
    const page = await browser.newPage({
      viewport: { width: Number(flags.width ?? 420), height: Number(flags.height ?? 860) },
      deviceScaleFactor: Number(flags.scale ?? 1),
      reducedMotion: flags["reduce-motion"] ? "reduce" : "no-preference",
    });
    page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
    for (const s of scenarios) {
      for (const theme of themes) {
        await page.goto(`${url}/?s=${s}&theme=${theme}${place}`);
        await page.waitForTimeout(Number(flags.wait ?? 1600));
        for (const click of clicks) {
          // a state a click opens: a menu, a list, a command's output
          await page.locator(click).first().click();
          await page.waitForTimeout(300);
        }
        if (flags.hover) {
          // a state the pointer shows: a row's buttons
          await page.locator(flags.hover).first().hover();
          await page.waitForTimeout(200);
        }
        if (flags.scroll === "top") {
          // the conversation from its start
          await page.evaluate(() => document.querySelector("[data-virtuoso-scroller]")?.scrollTo(0, 0));
          await page.waitForTimeout(400);
        }
        const suffix = `${flags.name ? `-${flags.name}` : ""}${themes.length > 1 || theme !== "dark" ? `-${theme}` : ""}`;
        const file = join(out, `${s}${suffix}.png`);
        await page.screenshot({ path: file });
        console.log(file);
      }
    }
  } finally {
    await browser.close();
    server.close();
    clean();
  }
}
