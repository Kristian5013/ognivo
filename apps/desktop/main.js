// The Electron main process of the ognivo GUI: one window around the UI of apps/ui, and
// the page's way to the core: a page that asks gets a port joined to the core's Unix
// socket, and the core starts when none answers (core.js, docs/architecture.md §3).
//
// Security (docs/architecture.md §10): the renderer is sandboxed and isolated and has no
// Node; it may neither navigate away nor open windows, and a link out of the app opens in
// the browser after a confirmation; the UI is served from the app:// scheme with a CSP
// that allows local code only and images from the app, data: and blob:; every permission
// request is refused.
//
//   OGNIVO_UI_DEV=http://127.0.0.1:5180   the UI's dev server instead of apps/ui/dist
//   OGNIVO_UI_QUERY='session=qwen.long-file&open=all'   a recorded session; `recorded` for all
//   OGNIVO_SCREENSHOT=out.png             render without showing a window, save, quit
//   OGNIVO_INACTIVE=1                     show the window without taking the focus (perf runs)
//
// Start it with start.js (npm start), which sets the GPU it renders on first (gpu.js); an
// installed window starts from packaging/launch, which does the same.

import { readFile, writeFile } from "node:fs/promises";
import { dirname, extname, join, normalize, sep } from "node:path";
import { fileURLToPath } from "node:url";

import {
  BrowserWindow,
  MessageChannelMain,
  app,
  dialog,
  ipcMain,
  protocol,
  screen,
  session,
  shell,
} from "electron";

import { bridge } from "./core.js";

const SCHEME = "app";
const HERE = dirname(fileURLToPath(import.meta.url));
const UI = join(HERE, "../ui/dist");
const DEV = process.env.OGNIVO_UI_DEV;
const QUERY = process.env.OGNIVO_UI_QUERY;
const SHOT = process.env.OGNIVO_SCREENSHOT;
const INACTIVE = process.env.OGNIVO_INACTIVE === "1";

const CSP = [
  "default-src 'self'",
  // Shiki's regular expressions run in WebAssembly
  "script-src 'self' 'wasm-unsafe-eval'",
  // the diff view styles its shadow roots; Radix sets inline styles
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "font-src 'self' data:",
  "worker-src 'self' blob:",
  "connect-src 'self'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
].join("; ");

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".wasm": "application/wasm",
};

protocol.registerSchemesAsPrivileged([
  {
    scheme: SCHEME,
    privileges: { standard: true, secure: true, supportFetchAPI: true, codeCache: true },
  },
]);

/** A file of apps/ui/dist, and nothing outside it. */
async function serve(request) {
  const url = new URL(request.url);
  const rel = decodeURIComponent(url.pathname).replace(/^\/+/, "") || "index.html";
  const path = normalize(join(UI, rel));
  if (url.host !== "ui" || !path.startsWith(UI + sep)) {
    return new Response("not found", { status: 404 });
  }
  try {
    const body = await readFile(path);
    return new Response(body, {
      headers: {
        "content-type": TYPES[extname(path)] ?? "application/octet-stream",
        "content-security-policy": CSP,
      },
    });
  } catch {
    return new Response("not found", { status: 404 });
  }
}

/** A link out of the app: in the browser, if the user says so; only http and https. */
async function openOutside(win, href) {
  let url;
  try {
    url = new URL(href);
  } catch {
    return;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return;
  const { response } = await dialog.showMessageBox(win, {
    type: "question",
    buttons: ["Open", "Cancel"],
    defaultId: 1,
    cancelId: 1,
    message: "Open this link in the browser?",
    detail: url.href,
  });
  if (response === 0) await shell.openExternal(url.href);
}

/** Where the app's own pages are. A URL's origin will not do: for app:// it is "null",
 * as for data: and file:. */
const HOME = DEV ? `${new URL(DEV).origin}/` : `${SCHEME}://ui/`;

function createWindow() {
  const start = `${DEV ?? `${SCHEME}://ui/index.html`}${QUERY ? `?${QUERY}` : ""}`;
  // a window as big as the screen's work area or bigger gets maximized by the window
  // manager: on a scaled laptop screen 1400x900 covered it all
  const area = screen.getPrimaryDisplay().workAreaSize;
  const win = new BrowserWindow({
    width: Math.min(1400, Math.round(area.width * 0.8)),
    height: Math.min(900, Math.round(area.height * 0.8)),
    minWidth: 800,
    minHeight: 500,
    title: "ognivo",
    backgroundColor: "#0f1012",
    show: false,
    webPreferences: {
      preload: join(HERE, "preload.cjs"),
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      webSecurity: true,
      spellcheck: false,
      // a hidden window gets no frames on Wayland; offscreen rendering needs no surface
      offscreen: Boolean(SHOT),
    },
  });
  win.webContents.setWindowOpenHandler(({ url }) => {
    void openOutside(win, url);
    return { action: "deny" };
  });
  win.webContents.on("will-navigate", (event, url) => {
    if (!url.startsWith(HOME)) {
      event.preventDefault();
      void openOutside(win, url);
    }
  });
  if (SHOT) {
    win.webContents.once("did-fail-load", (_event, code, reason, url) => {
      console.error(`ognivo: ${url} did not load: ${reason} (${code})`);
      app.exit(1);
    });
    win.webContents.once("did-finish-load", async () => {
      // the diff view highlights after the first paint
      await new Promise((resolve) => setTimeout(resolve, 1500));
      const image = await win.webContents.capturePage();
      await writeFile(SHOT, image.toPNG());
      app.quit();
    });
  }
  // shown once it has something to show; a perf run's window leaves the focus where it is
  win.once("ready-to-show", () => {
    if (SHOT) return;
    if (INACTIVE) win.showInactive();
    else win.show();
  });
  // the page's port to the core; only the app's own page gets one
  ipcMain.on("ognivo:core", (event) => {
    const frame = event.senderFrame;
    if (event.sender !== win.webContents || frame !== win.webContents.mainFrame || !frame.url.startsWith(HOME)) {
      return;
    }
    const { port1, port2 } = new MessageChannelMain();
    event.sender.postMessage("ognivo:core", null, [port2]);
    void bridge(port1);
  });
  void win.loadURL(start);
}

if (SHOT) {
  // a screenshot needs no GPU, and the GPU belongs to the model
  app.disableHardwareAcceleration();
} else if (!app.requestSingleInstanceLock()) {
  app.quit();
}

app.on("second-instance", () => {
  const [win] = BrowserWindow.getAllWindows();
  if (win?.isMinimized()) win.restore();
  win?.focus();
});

app.whenReady().then(() => {
  protocol.handle(SCHEME, serve);
  session.defaultSession.setPermissionRequestHandler((_contents, _permission, callback) =>
    callback(false),
  );
  session.defaultSession.setPermissionCheckHandler(() => false);
  createWindow();
});

app.on("window-all-closed", () => app.quit());
