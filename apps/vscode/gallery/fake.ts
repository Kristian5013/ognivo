// The extension and the core, faked for the gallery: the page of src/webview/ runs as it
// does in VS Code's webview, on a scenario's sessions (scenarios.ts), with the colours VS
// Code puts on a webview (themes/, taken by capture.mjs). `?s=` names the scenario,
// `?theme=light` the light theme.

import type { FromPage, InstructionFile, ToPage } from "../src/shared/messages.ts";
import * as P from "../src/shared/protocol.ts";
import { samePath } from "../src/shared/text.ts";
import { SCENARIOS } from "./scenarios.ts";
import dark from "./themes/dark.json";
import light from "./themes/light.json";

const query = new URLSearchParams(location.search);
const theme = query.get("theme") === "light" ? light : dark;
const scenario = SCENARIOS[query.get("s") ?? "welcome"] ?? SCENARIOS.welcome!;

// the theme, as VS Code puts it on a webview's page
document.documentElement.setAttribute("style", theme.style);
document.body.className = theme.bodyClass;
for (const [k, v] of Object.entries(theme.bodyAttrs)) document.body.setAttribute(k, v);
const defaults = document.createElement("style");
defaults.id = "_defaultStyles";
defaults.textContent = theme.defaultStyles;
document.head.prepend(defaults);

const toPage = (m: ToPage) => window.postMessage(m, "*");
const fromCore = (m: object) => toPage({ t: "core", data: JSON.stringify({ jsonrpc: "2.0", ...m }) });
const event = (e: P.Event) => fromCore({ method: P.NOTIFY_EVENT, params: e });

const project = scenario.folders[0] ?? "";
const events = scenario.events ?? [];
const lastSeq = () => Math.max(0, ...events.map((e) => e.seq));

const MCP: P.McpConfig = {
  files: [
    {
      scope: "user",
      path: "C:/Users/me/.ognivo/mcp.json",
      problem: null,
      servers: [
        // a key of nobody's, for the look of a key that is set
        { name: "exa", value: { command: "npx", args: ["-y", "exa-mcp-server"], env: { EXA_API_KEY: "00000000-0000-4000-8000-00000000c0de" } } },
        { name: "github", value: { type: "http", url: "https://api.githubcopilot.com/mcp/" } },
      ],
    },
    { scope: "project", path: `${project}/.mcp.json`, problem: null, servers: [] },
    { scope: "claude", path: "C:/Users/me/.claude.json", problem: null, servers: [] },
  ],
};

const SERVERS: P.McpServers = {
  servers: [
    {
      name: "github",
      source: "C:/Users/me/.ognivo/mcp.json",
      command: "https://api.githubcopilot.com/mcp/",
      remote: true,
      state: "sign_in",
      why: null,
      signed_in: false,
      instructions: null,
      tools: [],
      prompts: [],
    },
  ],
  problems: [],
};

function answer(method: string, params: Record<string, unknown>): unknown {
  switch (method) {
    case "initialize":
      return { protocol: P.VERSION, core: "0.0.1", exe: "", seq: lastSeq() };
    case "thread/list": {
      const only = params.project as string | null;
      const threads = scenario.threads.filter((t) => !only || samePath(t.project) === samePath(only));
      return { threads: threads.slice(0, (params.limit as number | null) ?? 200) };
    }
    case "thread/read": {
      const thread = params.thread as number;
      // a thread and its subagents' threads
      const subagents = new Set(
        events.flatMap((e) => (e.event === "thread_started" && e.info.parent?.thread === thread ? [e.thread] : [])),
      );
      return { events: events.filter((e) => e.thread === thread || subagents.has(e.thread)) };
    }
    case "thread/trajectory": {
      const trace = scenario.trajectories?.[params.thread as number];
      if (trace) return { trajectory: trace };
      return { trajectory: { schema_version: "ATIF-v1.4", session_id: "gallery", steps: [{ step_id: 1, source: "system", message: "You are ognivo, a coding agent.", extra: { conversation: 1, reason: "start" } }] } };
    }
    case "server/detect":
      if (scenario.detected === "failed" || !scenario.detected) throw new Error("the server does not answer: connection refused");
      return scenario.detected;
    case "mcp/config":
      return MCP;
    case "mcp/servers":
      return SERVERS;
    case "changes/list": {
      const running = scenario.threads.find((t) => t.thread === params.thread)?.running;
      if (running) throw Object.assign(new Error("a turn is running"), { code: P.ERRORS.busy });
      return { files: scenario.changes ?? [], commands: [], inherited: false };
    }
    default:
      // what changes the core's state answers as if it did
      return {};
  }
}

function request(method: string, params: Record<string, unknown>): unknown {
  switch (method) {
    case "exa/check":
      return { ok: true, says: "Exa takes the key: searches work" };
    case "sessions/delete":
      setTimeout(() => toPage({ t: "deleted", thread: params.thread as number }), 50);
      return true;
    case "instructions/list": {
      const files: InstructionFile[] = [
        { scope: "user", path: "C:/Users/me/.ognivo/AGENTS.md", shown: "~/.ognivo/AGENTS.md", exists: true, bytes: 61, text: "- Отвечай по-русски.\n- C и C++ собирай MSVC: `cl -nologo -W4 -EHsc main.cpp`.\n" },
        { scope: "claude", path: "C:/Users/me/.claude/CLAUDE.md", shown: "~/.claude/CLAUDE.md", exists: false, bytes: 0 },
        { scope: "project", path: `${project}/AGENTS.md`, shown: "AGENTS.md", exists: true, bytes: 1843 },
        { scope: "project", path: `${project}/CLAUDE.md`, shown: "CLAUDE.md", exists: false, bytes: 0 },
        { scope: "project", path: `${project}/.claude/CLAUDE.md`, shown: ".claude/CLAUDE.md", exists: false, bytes: 0 },
        { scope: "project", path: `${project}/CLAUDE.local.md`, shown: "CLAUDE.local.md", exists: false, bytes: 0 },
      ];
      return files;
    }
    default:
      return null;
  }
}

/** What follows the page's hello: the scenario's questions, its live events, its tab. */
function afterInitialize(): void {
  if (scenario.settings) setTimeout(() => toPage({ t: "settings", tab: scenario.settings }), 50);
  scenario.asks?.forEach((a, i) => setTimeout(() => fromCore({ id: 9000 + i, method: a.method, params: a.params }), 250));
  const live = scenario.live ?? [];
  live.forEach((e, i) => setTimeout(() => event(e), 400 + i * (scenario.every ?? 150)));
}

(window as unknown as { acquireVsCodeApi: () => unknown }).acquireVsCodeApi = () => ({
  postMessage(m: FromPage) {
    switch (m.t) {
      case "ready": {
        const state: Record<string, string> = {};
        if (scenario.show !== undefined) state[`ognivo.last.${samePath(project)}`] = String(scenario.show);
        toPage({
          t: "hello",
          folders: scenario.folders,
          state,
          place: scenario.place ?? "sidebar",
          session: {
            server: "http://127.0.0.1:8080",
            model: "",
            profile: "",
            reasoning: "",
            context: 0,
            mode: "auto",
            native: true,
            subagents: true,
            ...scenario.session,
          },
        });
        break;
      }
      case "core": {
        const r = JSON.parse(m.data) as { id?: number; method?: string; params?: Record<string, unknown>; result?: unknown };
        if (r.method === undefined || r.id === undefined) {
          console.log("answer of the page:", r);
          break;
        }
        try {
          fromCore({ id: r.id, result: answer(r.method, r.params ?? {}) });
        } catch (e) {
          const code = (e as { code?: number }).code ?? -32000;
          fromCore({ id: r.id, error: { code, message: e instanceof Error ? e.message : String(e) } });
        }
        if (r.method === "initialize") afterInitialize();
        break;
      }
      case "request":
        toPage({ t: "response", id: m.id, result: request(m.method, (m.params ?? {}) as Record<string, unknown>) });
        break;
      default:
        console.log("the page says:", m);
    }
  },
});
