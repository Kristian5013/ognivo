import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app.tsx";
import { Core, corePort } from "./core.ts";
import "./index.css";
import { LiveApp } from "./live-app.tsx";
import { Threads } from "./threads.ts";

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
const view = createRoot(root);

// a recorded session, the bench or a screenshot of one names it in the URL; otherwise, in
// Electron, the GUI is the core's
const params = new URLSearchParams(location.search);
const recorded = ["session", "bench", "stream", "recorded"].some((p) => params.has(p));
const port = recorded ? undefined : corePort();

async function connect(port: Promise<MessagePort>): Promise<void> {
  view.render(<p className="m-auto text-muted-foreground">Connecting to the core…</p>);
  try {
    const core = new Core(await port);
    // before the stream starts: it starts with the answer
    const threads = new Threads(core);
    await core.initialize("ognivo-gui 0.0.1");
    view.render(
      <StrictMode>
        <LiveApp core={core} threads={threads} />
      </StrictMode>,
    );
  } catch (e) {
    view.render(
      <div className="m-auto max-w-lg space-y-3 px-6 text-center">
        <p>The core did not answer: {e instanceof Error ? e.message : String(e)}</p>
        <button type="button" onClick={() => location.reload()} className="rounded border border-border px-3 py-1 hover:bg-raised">
          Try again
        </button>
      </div>,
    );
  }
}

if (port) {
  void connect(port);
} else {
  view.render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
