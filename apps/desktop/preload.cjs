// The page's way to the core: `window.ognivo.connect()` asks main for a port to it
// (core.js), and the port comes to the page as a window message `ognivo:core`. Besides,
// `zoom(factor)` scales the page, as the settings' scale says. The page gets nothing else
// of Node or Electron.

const { contextBridge, ipcRenderer, webFrame } = require("electron");

ipcRenderer.on("ognivo:core", (event) => window.postMessage("ognivo:core", "*", event.ports));
contextBridge.exposeInMainWorld("ognivo", {
  connect: () => ipcRenderer.send("ognivo:core"),
  zoom: (factor) => {
    if (typeof factor === "number" && factor >= 0.8 && factor <= 1.4) webFrame.setZoomFactor(factor);
  },
});
