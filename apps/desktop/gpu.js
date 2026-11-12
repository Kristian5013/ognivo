// The GPU the window renders on (docs/architecture.md §10): the one that drives the
// screen, not the one the model needs.
//
// On a hybrid laptop GLVND tries NVIDIA's EGL first, and it takes Wayland displays too:
// Electron's GPU process loaded libnvidia-eglcore and rendered on the RTX while the
// screen hangs on the Intel. With __EGL_VENDOR_LIBRARY_FILENAMES set to Mesa's vendor
// file only Mesa loads. It has to be in the environment before Electron starts: Chromium
// forks the GPU process from a zygote that exists before main.js runs. An installed window
// gets it from packaging/launch, which reads the screens the same way.

import { existsSync, readFileSync, readdirSync } from "node:fs";

const MESA = "/usr/share/glvnd/egl_vendor.d/50_mesa.json";
const DRM = "/sys/class/drm";

/** The PCI vendors of the GPUs behind the connected screens: `0x8086` Intel, `0x1002`
 * AMD, `0x10de` NVIDIA. */
export function screenVendors() {
  try {
    return readdirSync(DRM)
      .filter((c) => /^card\d+-/.test(c))
      .filter((c) => readFileSync(`${DRM}/${c}/status`, "utf8").trim() === "connected")
      .map((c) => readFileSync(`${DRM}/${c}/device/device/vendor`, "utf8").trim());
  } catch {
    return [];
  }
}

/** What to add to Electron's environment: Mesa's EGL when every connected screen hangs on
 * a Mesa GPU, nothing when a screen is on the NVIDIA (a MUX in dGPU mode), on another
 * system, or when the user chose a vendor already. */
export function gpuEnv(env = process.env) {
  if (process.platform !== "linux" || env.__EGL_VENDOR_LIBRARY_FILENAMES || !existsSync(MESA)) {
    return {};
  }
  const vendors = screenVendors();
  const mesa = vendors.length > 0 && vendors.every((v) => v === "0x8086" || v === "0x1002");
  return mesa ? { __EGL_VENDOR_LIBRARY_FILENAMES: MESA } : {};
}
