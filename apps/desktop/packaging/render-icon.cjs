// Renders ognivo.svg at the sizes of a Windows icon, offscreen and without the GPU, as one
// .ico of PNG images: how ognivo.ico beside it was made. From the repository's root:
//
//   env -u ELECTRON_RUN_AS_NODE node_modules/electron/dist/electron --disable-gpu \
//     apps/desktop/packaging/render-icon.cjs apps/desktop/packaging/ognivo.svg apps/desktop/packaging/ognivo.ico

const { app, BrowserWindow } = require("electron");
const fs = require("node:fs");

const [svgPath, icoPath] = process.argv.slice(-2);
const svg = fs.readFileSync(svgPath);
const SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256];

app.disableHardwareAcceleration();
app.whenReady().then(async () => {
  const win = new BrowserWindow({
    width: 256, height: 256, show: false, frame: false, transparent: true,
    useContentSize: true, webPreferences: { offscreen: true },
  });
  const html = `<html><body style="margin:0;background:transparent"><img style="display:block" width="256" height="256" src="data:image/svg+xml;base64,${svg.toString("base64")}"></body></html>`;
  await win.loadURL(`data:text/html;base64,${Buffer.from(html).toString("base64")}`);
  await new Promise((r) => setTimeout(r, 500));
  const shot = await win.webContents.capturePage({ x: 0, y: 0, width: 256, height: 256 });
  const full = shot.resize({ width: 256, height: 256, quality: "best" });
  const images = SIZES.map((size) => [size, size === 256 ? full.toPNG() : full.resize({ width: size, height: size, quality: "best" }).toPNG()]);
  win.destroy();
  const head = Buffer.alloc(6 + 16 * images.length);
  head.writeUInt16LE(0, 0);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(images.length, 4);
  let offset = head.length;
  images.forEach(([size, png], i) => {
    const e = 6 + 16 * i;
    head.writeUInt8(size >= 256 ? 0 : size, e);
    head.writeUInt8(size >= 256 ? 0 : size, e + 1);
    head.writeUInt8(0, e + 2);
    head.writeUInt8(0, e + 3);
    head.writeUInt16LE(1, e + 4);
    head.writeUInt16LE(32, e + 6);
    head.writeUInt32LE(png.length, e + 8);
    head.writeUInt32LE(offset, e + 12);
    offset += png.length;
  });
  fs.writeFileSync(icoPath, Buffer.concat([head, ...images.map(([, png]) => png)]));
  app.quit();
});
