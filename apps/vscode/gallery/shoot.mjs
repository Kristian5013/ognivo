// The panel's states as pictures: builds the gallery (the panel's page on a fake extension
// and core, fake.ts), serves it, and takes each scenario (scenarios.ts) in the system's
// Edge or Chrome, at a side bar's width.
//
//   node gallery/shoot.mjs [scenario...] [--theme=dark|light|both] [--width=420]
//                          [--height=860] [--out=dir] [--serve]
//
// `--serve` only serves it, for a browser: http://127.0.0.1:<port>/?s=session&theme=light

import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "@playwright/test";
import * as esbuild from "esbuild";

const here = fileURLToPath(new URL(".", import.meta.url));
const dist = join(here, "dist");
const flags = Object.fromEntries(
  process.argv
    .slice(2)
    .filter((a) => a.startsWith("--"))
    .map((a) => {
      // a value may hold `=` itself: `--click=button >> nth=1`
      const at = a.indexOf("=");
      return at < 0 ? [a.slice(2), "true"] : [a.slice(2, at), a.slice(at + 1)];
    }),
);
const named = process.argv.slice(2).filter((a) => !a.startsWith("--"));

rmSync(dist, { recursive: true, force: true });
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
} else {
  const source = readFileSync(join(here, "scenarios.ts"), "utf8");
  const all = [...source.matchAll(/^ {2}"?([a-z-]+)"?: [{r]/gm)].map((m) => m[1]);
  const scenarios = named.length ? named : all;
  const themes = flags.theme === "both" ? ["dark", "light"] : [flags.theme ?? "dark"];
  const out = flags.out ?? join(here, "shots");
  mkdirSync(out, { recursive: true });
  const browser = await chromium.launch({ channel: process.env.OGNIVO_BROWSER ?? (process.platform === "win32" ? "msedge" : "chrome") });
  try {
    const page = await browser.newPage({
      viewport: { width: Number(flags.width ?? 420), height: Number(flags.height ?? 860) },
      deviceScaleFactor: Number(flags.scale ?? 1),
    });
    page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
    for (const s of scenarios) {
      for (const theme of themes) {
        await page.goto(`${url}/?s=${s}&theme=${theme}`);
        await page.waitForTimeout(Number(flags.wait ?? 1600));
        if (flags.click) {
          // a state a click opens: a menu, a list
          await page.locator(flags.click).first().click();
          await page.waitForTimeout(300);
        }
        if (flags.scroll === "top") {
          // the conversation from its start
          await page.evaluate(() => document.querySelector("[data-virtuoso-scroller]")?.scrollTo(0, 0));
          await page.waitForTimeout(400);
        }
        const file = join(out, `${s}${themes.length > 1 || theme !== "dark" ? `-${theme}` : ""}.png`);
        await page.screenshot({ path: file });
        console.log(file);
      }
    }
  } finally {
    await browser.close();
    server.close();
  }
}
