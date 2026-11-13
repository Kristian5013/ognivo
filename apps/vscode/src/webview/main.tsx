// The agent's page in VS Code's webview: the extension's hello first, then a client of the
// core on a port through the extension, and the panel on it.

import "@vscode/codicons/dist/codicon.css";
import "./panel.css";

import { createRoot } from "react-dom/client";

import { Core } from "../shared/rpc.ts";
import { App } from "./app.tsx";
import { hello, port, run } from "./host.ts";
import { Icon } from "./icon.tsx";
import { Threads } from "./model/threads.ts";
import { Spark } from "./welcome.tsx";

const element = document.getElementById("root");
if (!element) throw new Error("the page has no #root");
const root = createRoot(element);
root.render(
  <div className="centered muted">
    <Spark size={28} />
    <p>Connecting to the core…</p>
  </div>,
);

void hello().then(async (h) => {
  const core = new Core(port());
  // before the stream starts: it starts with the answer
  const threads = new Threads(core);
  try {
    await core.initialize("ognivo-vscode 0.0.1");
    root.render(<App core={core} threads={threads} hello={h} />);
  } catch (e) {
    root.render(
      <div className="centered">
        <Icon name="debug-disconnect" className="centered-icon" />
        <p>The core did not answer: {e instanceof Error ? e.message : String(e)}</p>
        <div className="buttons">
          <button type="button" className="btn primary" onClick={() => location.reload()}>
            Try again
          </button>
          <button type="button" className="btn ghost" onClick={() => run("showLog")}>
            The core's log
          </button>
          <button type="button" className="btn ghost" onClick={() => run("vscodeSettings")}>
            Settings
          </button>
        </div>
      </div>,
    );
  }
});
