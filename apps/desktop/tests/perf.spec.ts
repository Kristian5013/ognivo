import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { _electron as electron, expect, test } from "@playwright/test";
import electronBinary from "electron";

import { gpuEnv, screenVendors } from "../gpu.js";

// The budgets of §10: a stream keeps 60 fps on a thread of 10k items, and input answers
// within 16 ms, on the GPU that drives the screen. The window is real (frames are what
// the GPU shows) and does not take the focus; the stream is the longest recorded session
// at four times its pace, with every item open, so reasoning renders as it streams, while
// the user types.

interface Probe {
  frames: number[];
  keys: number[];
  slow: { name: string; duration: number; handled: number }[];
  streaming: boolean;
}

declare global {
  interface Window {
    __ognivoPerf?: Probe;
  }
}

const main = fileURLToPath(new URL("../main.js", import.meta.url));
const FRAME = 1000 / 60;

/** The GPU drivers this Electron's GPU processes loaded (VS Code and Chrome run other
 * binaries): what tells which GPU renders, where getGPUInfo's "active" does not. */
function gpuDrivers(): string[] {
  const found = new Set<string>();
  for (const pid of readdirSync("/proc").filter((d) => /^\d+$/.test(d))) {
    try {
      // Chromium's zygote children set their title: the arguments are joined by spaces
      const cmd = readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ");
      if (!cmd.startsWith(`${electronBinary} `) || !cmd.includes(" --type=gpu-process")) continue;
      const maps = readFileSync(`/proc/${pid}/maps`, "utf8");
      for (const m of maps.matchAll(/(libnvidia-eglcore|libnvidia-glcore|libgallium|iris_dri)[^/\s]*/g)) {
        found.add(m[1]!);
      }
    } catch {
      // gone, or not ours to read
    }
  }
  return [...found].sort();
}

function at(sorted: number[], p: number): number {
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))] ?? 0;
}

test("a stream at the end of 10k items keeps 60 fps, and typing answers within a frame", async () => {
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined) env[k] = v;
  // an editor's extension host may pass this on; Electron would start as plain Node
  delete env.ELECTRON_RUN_AS_NODE;
  Object.assign(env, gpuEnv());
  env.OGNIVO_UI_QUERY = "bench=10000&stream=qwen.probing&speed=4&open=all";
  env.OGNIVO_INACTIVE = "1";
  const app = await electron.launch({ args: [main], env });
  try {
    const page = await app.firstWindow();
    await page.waitForFunction(() => window.__ognivoPerf?.streaming === true, null, {
      timeout: 60_000,
    });
    // the budget is the stream and the typing, measured once they run: not the load, and
    // not the click that activates the window, when Wayland sends no frame for some 50 ms
    // (a trace shows the renderer and the GPU idle then, and no BeginFrame)
    await page.click("textarea");
    await page.waitForTimeout(1000);
    await page.evaluate(() => {
      window.__ognivoPerf!.frames.length = 0;
      window.__ognivoPerf!.keys.length = 0;
      window.__ognivoPerf!.slow.length = 0;
    });
    await page.keyboard.type("does the window keep up while the model streams?", { delay: 60 });
    await page.waitForTimeout(2000);
    const probe = await page.evaluate(() => ({ ...window.__ognivoPerf! }));
    const hz = await app.evaluate(({ screen }) => screen.getPrimaryDisplay().displayFrequency);
    const drivers = gpuDrivers();

    const sorted = [...probe.frames].sort((a, b) => a - b);
    const over = probe.frames.filter((f) => f > FRAME + 1).length;
    const keys = [...probe.keys].sort((a, b) => a - b);
    const painted = probe.slow.filter((e) => e.name.startsWith("key") || e.name === "input");
    const screens = screenVendors();
    console.log(
      JSON.stringify(
        {
          frames: probe.frames.length,
          p50: at(sorted, 0.5).toFixed(1),
          p95: at(sorted, 0.95).toFixed(1),
          p99: at(sorted, 0.99).toFixed(1),
          max: (sorted.at(-1) ?? 0).toFixed(1),
          over60fps: `${over} (${((over / Math.max(1, probe.frames.length)) * 100).toFixed(1)}%)`,
          keys: keys.length,
          keyToFrameMax: (keys.at(-1) ?? 0).toFixed(1),
          keyToFrameP95: at(keys, 0.95).toFixed(1),
          paintedAfter16ms: painted.length,
          handledMax: Math.max(0, ...painted.map((k) => k.handled)).toFixed(1),
          handledOver16: painted
            .filter((k) => k.handled >= 16)
            .map((k) => `${k.name} ${k.handled.toFixed(0)}/${k.duration}`),
          display: `${hz} Hz`,
          drivers,
          screens,
        },
        null,
        1,
      ),
    );
    // a covered window gets no frames: then there is nothing to judge
    expect(probe.frames.length, "the window stayed in view, 60 frames a second or more").toBeGreaterThan(
      60 * 2.5,
    );
    expect(probe.streaming, "the stream ran through the measurement").toBe(true);
    expect(at(sorted, 0.95), "95% of frames within 1/60 s").toBeLessThanOrEqual(FRAME + 1);
    expect(keys.length, "keys were measured").toBeGreaterThan(20);
    expect(keys.at(-1) ?? 0, "every key in a frame within 16 ms").toBeLessThan(16);
    expect(drivers.length, "the GPU process was found").toBeGreaterThan(0);
    if (screens.length && !screens.includes("0x10de")) {
      expect(drivers, "rendered on the GPU of the screen: no NVIDIA driver loaded").not.toContain(
        "libnvidia-eglcore",
      );
    }
  } finally {
    await app.close();
  }
});
