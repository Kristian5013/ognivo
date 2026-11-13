// The agent's page in VS Code's webview: the extension's hello first, then a client of the
// core on a port through the extension, and the panel on it.

import "@vscode/codicons/dist/codicon.css";
import "./panel.css";

import { createRoot } from "react-dom/client";

import { Core } from "../shared/rpc.ts";
import { App } from "./app.tsx";
import { hello, port, run } from "./host.ts";
import { Threads } from "./model/threads.ts";

const element = document.getElementById("root");
if (!element) throw new Error("the page has no #root");
const root = createRoot(element);
root.render(<p className="centered muted">Connecting to the core…</p>);

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
        <p>The core did not answer: {e instanceof Error ? e.message : String(e)}</p>
        <div className="buttons">
          <button type="button" className="primary" onClick={() => location.reload()}>
            Try again
          </button>
          <button type="button" className="link" onClick={() => run("settings")}>
            Settings
          </button>
        </div>
      </div>,
    );
  }
});
