// Starts the GUI: Electron with main.js, its environment set first (gpu.js), because
// main.js runs too late to set it.

import { spawn } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import electron from "electron";

import { gpuEnv } from "./gpu.js";

const env = { ...process.env, ...gpuEnv() };
// an editor's extension host may pass this on; Electron would start as plain Node
delete env.ELECTRON_RUN_AS_NODE;

const child = spawn(electron, [dirname(fileURLToPath(import.meta.url)), ...process.argv.slice(2)], {
  stdio: "inherit",
  env,
});
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => child.kill(signal));
child.on("exit", (code, signal) => process.exit(code ?? (signal ? 1 : 0)));
