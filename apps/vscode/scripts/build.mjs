// Builds the extension into dist/: the extension's code for VS Code's Node
// (dist/extension.cjs, with `vscode` left to VS Code) and the panel's page for its webview
// (dist/webview/: main.js and its chunks, main.css, the codicons' font).
//
//   npm run build -w apps/vscode            once, minified
//   npm run watch -w apps/vscode            again on each change, not minified

import { rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import * as esbuild from "esbuild";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const watch = process.argv.includes("--watch");

/** @type {import("esbuild").BuildOptions} */
const host = {
  entryPoints: [join(root, "src/extension.ts")],
  outfile: join(root, "dist/extension.cjs"),
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node22",
  external: ["vscode"],
  // maps while developing; a package has none
  sourcemap: watch,
  minify: !watch,
  logLevel: "info",
};

/** @type {import("esbuild").BuildOptions} */
const page = {
  entryPoints: [join(root, "src/webview/main.tsx")],
  outdir: join(root, "dist/webview"),
  bundle: true,
  platform: "browser",
  format: "esm",
  splitting: true,
  chunkNames: "chunks/[name]-[hash]",
  target: "es2022",
  jsx: "automatic",
  loader: { ".ttf": "file" },
  define: { "process.env.NODE_ENV": JSON.stringify(watch ? "development" : "production") },
  sourcemap: watch,
  minify: !watch,
  logLevel: "info",
};

rmSync(join(root, "dist"), { recursive: true, force: true });
if (watch) {
  const contexts = await Promise.all([esbuild.context(host), esbuild.context(page)]);
  await Promise.all(contexts.map((c) => c.watch()));
} else {
  await Promise.all([esbuild.build(host), esbuild.build(page)]);
}
