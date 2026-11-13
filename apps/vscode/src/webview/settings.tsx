// The panel's settings (docs/architecture.md §10), in three tabs:
// - Model: what a new session works with, the extension's `ognivo.*` settings;
// - MCP: the servers of the user, the project and Claude Code's config, with Exa ready to
//   switch on with a key; on and off, added and removed in the config files (the core
//   writes them: `mcp/config/set`); the session's servers as they run, with their sign-ins;
// - Instructions: what the agent gets besides its prompt: the user's own instructions,
//   edited here, Claude Code's and the project's files, and the exact system prompt of the
//   session on show.

import { useEffect, useState } from "react";

import type { InstructionFile, NewSession, SESSION_SETTINGS, SettingsTab } from "../shared/messages.ts";
import { entry } from "../shared/mcpform.ts";
import profiles from "../shared/profiles.json";
import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";
import { forget, useDetected } from "./detect.ts";
import { openLink, request, run } from "./host.ts";
import type { AtifTrace } from "./model/atif.ts";
import { type Budget, contextChoices, contextSize, windowChoice } from "./model/context.ts";
import { MODES } from "./model/modes.ts";

type Profile = { context: Budget; reasoning: { levels: string[]; default: string }; native: boolean };
const PROFILES = profiles as Record<string, Profile>;

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** A switch: a checkbox that reads as on and off. */
function Switch({ on, onChange, label, disabled }: { on: boolean; onChange: (on: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      title={label}
      disabled={disabled}
      className={`switch ${on ? "on" : ""}`}
      onClick={() => onChange(!on)}
    >
      <span />
    </button>
  );
}

// ---------------------------------------------------------------- Model

function ModelTab({ core, session }: { core: Core; session: NewSession }) {
  const [server, setServer] = useState(session.server);
  const [error, setError] = useState<string>();
  useEffect(() => setServer(session.server), [session.server]);
  const detected = useDetected(core, session.server, session.model);
  const d = detected && detected !== "failed" ? detected : undefined;
  const set = (key: (typeof SESSION_SETTINGS)[number], value: unknown) =>
    request("settings/set", { key, value }).then(
      () => setError(undefined),
      (e: unknown) => setError(message(e)),
    );
  const profileId = session.profile || d?.profile || "";
  const profile = PROFILES[profileId];
  const levels = session.profile ? (profile?.reasoning.levels ?? []) : (d?.levels ?? profile?.reasoning.levels ?? []);
  const defaultLevel = session.profile ? profile?.reasoning.default : (d?.default_level ?? profile?.reasoning.default);
  const window = d?.window ?? undefined;
  const choices = contextChoices(profile?.context, window);
  const native = profile?.native ?? d?.native;
  return (
    <div className="tab-body">
      <label className="field">
        <span>llama-server</span>
        <span className="field-row">
          <input
            value={server}
            onChange={(e) => setServer(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
            onBlur={() => {
              const url = server.trim();
              if (url && url !== session.server) {
                forget();
                void set("server", url);
              }
            }}
            spellCheck={false}
            className="mono"
          />
          <button
            type="button"
            className="secondary small"
            title="Ask the server again what it runs"
            onClick={() => {
              forget();
              void set("server", server.trim());
            }}
          >
            Check
          </button>
        </span>
        <small className={detected === "failed" ? "err" : "muted"}>
          {detected === undefined
            ? "asking the server…"
            : detected === "failed"
              ? "the server does not answer: is llama-server running there?"
              : `${d?.model ?? "its model"}${d?.profile ? ` · profile ${d.profile}` : " · no profile fits it: the Generic path guesses"}${d?.window ? ` · window ${contextSize(d.window)}` : ""}`}
        </small>
      </label>
      {d && d.models.length > 0 && (
        <label className="field">
          <span>Model behind the router</span>
          <select value={session.model} onChange={(e) => void set("model", e.target.value)}>
            <option value="">the one the router runs</option>
            {d.models.map((m) => (
              <option key={m.id} value={m.id}>
                {m.id} · {m.status === "loaded" ? "loaded" : m.status}
                {m.profile ? "" : " · no profile"}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className="field">
        <span>Profile</span>
        <select value={session.profile} onChange={(e) => void set("profile", e.target.value)}>
          <option value="">{d?.profile ? `the server's model: ${d.profile}` : "the server's model"}</option>
          {Object.keys(PROFILES).map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </label>
      <label className="field">
        <span>Reasoning</span>
        <select value={levels.includes(session.reasoning) ? session.reasoning : ""} onChange={(e) => void set("reasoning", e.target.value)}>
          <option value="">{defaultLevel ? `the model's default: ${defaultLevel}` : "the model's default"}</option>
          {levels.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
        <small className="muted">Higher levels think longer: more time and context, not fewer mistakes of memory.</small>
      </label>
      <label className="field">
        <span>Context</span>
        <select value={choices.includes(session.context) ? session.context : 0} onChange={(e) => void set("context", Number(e.target.value))}>
          <option value={0}>{windowChoice(profile?.context, window)}</option>
          {choices.map((n) => (
            <option key={n} value={n}>
              {contextSize(n)}
              {n === profile?.context.working_max ? ": what the profile was measured at" : ""}
            </option>
          ))}
        </select>
      </label>
      <label className="field">
        <span>Mode of a new session</span>
        <select value={session.mode} onChange={(e) => void set("mode", e.target.value)}>
          {MODES.map((m) => (
            <option key={m.mode} value={m.mode}>
              {m.glyph} {m.label}
            </option>
          ))}
        </select>
        <small className="muted">{MODES.find((m) => m.mode === session.mode)?.says}</small>
      </label>
      <label className="check">
        <input type="checkbox" checked={session.native && native !== false} disabled={native === false} onChange={(e) => void set("native", e.target.checked)} />
        <span>
          Native path: ognivo renders the model's format itself
          <small className="muted">{native === false ? ` · not for ${profileId} yet` : " (gpt-oss, Qwen)"}</small>
        </span>
      </label>
      <label className="check">
        <input type="checkbox" checked={session.subagents} onChange={(e) => void set("subagents", e.target.checked)} />
        <span>
          Subagents: the agent may hand parts of a task to agents of its own
          <small className="muted"> (llama-server -np 2 or more)</small>
        </span>
      </label>
      {error && <p className="error">{error}</p>}
      <p className="muted small-print">
        These start each new session; a session keeps what it started with.{" "}
        <button type="button" className="link" onClick={() => run("vscodeSettings")}>
          VS Code's settings of ognivo
        </button>
      </p>
    </div>
  );
}

// ---------------------------------------------------------------- MCP

/** The command line or URL of an entry, as it reads. */
function target(value: Record<string, unknown>): string {
  if (typeof value.url === "string") return value.url.replace(/([?&](?:exaApiKey|api[_-]?key|token)=)[^&]+/gi, "$1•••");
  const args = Array.isArray(value.args) ? value.args.map(String) : [];
  return [String(value.command ?? ""), ...args].join(" ");
}

const SCOPE: Record<string, string> = {
  user: "yours, every project",
  project: "this project",
  claude: "Claude Code's",
};

/** Exa: web search and page reads for the agent, Exa's own MCP server. */
const EXA = "exa";
const EXA_KEY = "EXA_API_KEY";

function exaKey(value: Record<string, unknown> | undefined): string {
  const env = value?.env as Record<string, unknown> | undefined;
  const fromEnv = typeof env?.[EXA_KEY] === "string" ? (env[EXA_KEY] as string) : "";
  const fromUrl = typeof value?.url === "string" ? (/[?&]exaApiKey=([^&]+)/.exec(value.url)?.[1] ?? "") : "";
  return fromEnv || fromUrl;
}

function ServerState({ s }: { s: P.McpServerState | undefined }) {
  if (!s) return null;
  const [cls, text] =
    s.state === "running"
      ? ["ok", `running · ${s.tools.length} tool${s.tools.length === 1 ? "" : "s"}${s.prompts.length ? `, ${s.prompts.length} prompt${s.prompts.length === 1 ? "" : "s"}` : ""}`]
      : s.state === "sign_in"
        ? ["warn", "wants a sign-in"]
        : s.state === "untrusted"
          ? ["muted", "waits for your trust"]
          : ["err", `did not start${s.why ? `: ${s.why}` : ""}`];
  return <small className={`state ${cls}`}>{text}</small>;
}

function ExaCard({
  value,
  scope,
  live,
  onWrite,
}: {
  value: Record<string, unknown> | undefined;
  scope: string | undefined;
  live: P.McpServerState | undefined;
  onWrite: (value: Record<string, unknown>) => Promise<void>;
}) {
  const has = exaKey(value);
  const on = Boolean(value) && value?.disabled !== true;
  const [key, setKey] = useState("");
  const [asking, setAsking] = useState(false);
  const [error, setError] = useState<string>();
  const mine = !scope || scope === "user";
  const save = (k: string) => {
    const base: Record<string, unknown> = value && mine ? { ...value } : { command: "npx", args: ["-y", "exa-mcp-server"] };
    delete base.disabled;
    if (typeof base.url === "string") base.url = base.url.replace(/([?&]exaApiKey=)[^&]+/, `$1${encodeURIComponent(k)}`);
    else base.env = { ...((base.env as Record<string, unknown>) ?? {}), [EXA_KEY]: k };
    return onWrite(base).then(
      () => {
        setKey("");
        setAsking(false);
        setError(undefined);
      },
      (e: unknown) => setError(message(e)),
    );
  };
  const toggle = (next: boolean) => {
    if (!next && value) return void onWrite({ ...value, disabled: true }).catch((e: unknown) => setError(message(e)));
    if (has && value) {
      const v = { ...value };
      delete v.disabled;
      return void onWrite(v).catch((e: unknown) => setError(message(e)));
    }
    setAsking(true);
  };
  return (
    <div className="card preset">
      <div className="card-head">
        <span className="codicon codicon-globe" />
        <span className="card-title">Exa</span>
        <span className="muted">web search and pages for the agent</span>
        <span className="spacer" />
        <Switch on={on} onChange={toggle} label={on ? "Switch Exa off" : "Switch Exa on"} disabled={!mine} />
      </div>
      <p className="muted">
        The agent searches the web and reads pages through Exa's MCP server (<code>npx exa-mcp-server</code>, which needs Node.js).
        Queries go to Exa with your key.
      </p>
      {!mine && <p className="muted">Configured in {SCOPE[scope ?? ""] ?? scope} config: change it there.</p>}
      {mine && (asking || on || has) && (
        <form
          className="field-row"
          onSubmit={(e) => {
            e.preventDefault();
            if (key.trim()) void save(key.trim());
          }}
        >
          <input
            type="password"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder={has ? "a key is set: paste another to change it" : "your Exa API key"}
            aria-label="Exa API key"
            autoFocus={asking}
            spellCheck={false}
          />
          <button type="submit" className="primary small" disabled={!key.trim()}>
            {has ? "Change" : "Save and switch on"}
          </button>
        </form>
      )}
      <div className="card-foot">
        <button type="button" className="link" onClick={() => openLink("https://dashboard.exa.ai/api-keys")}>
          Get a key at exa.ai
        </button>
        <ServerState s={live} />
        {on && !live && <small className="muted">starts with the next session</small>}
      </div>
      {error && <p className="error">{error}</p>}
    </div>
  );
}

function ServerRow({
  scope,
  name,
  value,
  live,
  project,
  core,
  onWrite,
}: {
  scope: string;
  name: string;
  value: Record<string, unknown>;
  live: P.McpServerState | undefined;
  project: string;
  core: Core;
  onWrite: (value: Record<string, unknown> | null) => Promise<void>;
}) {
  const [removing, setRemoving] = useState(false);
  const [error, setError] = useState<string>();
  const on = value.disabled !== true;
  const editable = scope !== "claude";
  const act = (p: Promise<unknown>) => p.catch((e: unknown) => setError(message(e)));
  return (
    <div className={`server ${on ? "" : "off"}`}>
      <div className="server-head">
        <Switch
          on={on}
          disabled={!editable}
          label={on ? `Switch ${name} off` : `Switch ${name} on`}
          onChange={(next) => {
            const v = { ...value };
            if (next) delete v.disabled;
            else v.disabled = true;
            void act(onWrite(v));
          }}
        />
        <span className="server-name">{name}</span>
        <span className="badge">{SCOPE[scope] ?? scope}</span>
        <span className="spacer" />
        {live?.remote &&
          (live.signed_in ? (
            <button type="button" className="link" onClick={() => void act(core.call("mcp/logout", { project, server: name }))}>
              Sign out
            </button>
          ) : (
            <button type="button" className="link" onClick={() => void act(core.call("mcp/login", { project, server: name }))}>
              Sign in
            </button>
          ))}
        {editable &&
          (removing ? (
            <>
              <button type="button" className="secondary small" onClick={() => void act(onWrite(null))}>
                Remove
              </button>
              <button type="button" className="link" onClick={() => setRemoving(false)}>
                Keep
              </button>
            </>
          ) : (
            <button type="button" className="icon-button" title={`Remove ${name}`} onClick={() => setRemoving(true)}>
              <span className="codicon codicon-trash" />
            </button>
          ))}
      </div>
      <code className="server-target">{target(value)}</code>
      <ServerState s={live} />
      {error && <p className="error">{error}</p>}
    </div>
  );
}

function AddServer({ onAdd }: { onAdd: (scope: "user" | "project", name: string, value: Record<string, unknown>) => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [scope, setScope] = useState<"user" | "project">("user");
  const [kind, setKind] = useState<"command" | "url">("command");
  const [what, setWhat] = useState("");
  const [extra, setExtra] = useState("");
  const [error, setError] = useState<string>();
  if (!open) {
    return (
      <button type="button" className="secondary small add" onClick={() => setOpen(true)}>
        <span className="codicon codicon-add" /> Add a server
      </button>
    );
  }
  return (
    <form
      className="card add-form"
      onSubmit={(e) => {
        e.preventDefault();
        if (!/^[\w.-]+$/.test(name)) return setError("a name of letters, digits, '.', '_' or '-'");
        const v = entry(kind, what, extra);
        if (typeof v === "string") return setError(v);
        onAdd(scope, name, v).then(
          () => {
            setOpen(false);
            setName("");
            setWhat("");
            setExtra("");
            setError(undefined);
          },
          (e: unknown) => setError(message(e)),
        );
      }}
    >
      <label className="field">
        <span>Name</span>
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="github" autoFocus spellCheck={false} />
      </label>
      <div className="field">
        <span>For</span>
        <span className="radios">
          <label>
            <input type="radio" checked={scope === "user"} onChange={() => setScope("user")} /> me, every project (~/.ognivo/mcp.json)
          </label>
          <label>
            <input type="radio" checked={scope === "project"} onChange={() => setScope("project")} /> this project (.mcp.json)
          </label>
        </span>
      </div>
      <div className="field">
        <span>Runs as</span>
        <span className="radios">
          <label>
            <input type="radio" checked={kind === "command"} onChange={() => setKind("command")} /> a command on this machine
          </label>
          <label>
            <input type="radio" checked={kind === "url"} onChange={() => setKind("url")} /> a server at a URL
          </label>
        </span>
      </div>
      <label className="field">
        <span>{kind === "command" ? "Command line" : "URL"}</span>
        <input
          value={what}
          onChange={(e) => setWhat(e.target.value)}
          placeholder={kind === "command" ? "npx -y @modelcontextprotocol/server-filesystem ." : "https://example.com/mcp"}
          spellCheck={false}
          className="mono"
        />
      </label>
      <label className="field">
        <span>{kind === "command" ? "Environment: KEY=value, a line each" : "Headers: Name: value, a line each"}</span>
        <textarea value={extra} onChange={(e) => setExtra(e.target.value)} rows={2} spellCheck={false} className="mono" />
      </label>
      {error && <p className="error">{error}</p>}
      <div className="buttons">
        <button type="submit" className="primary small">
          Add
        </button>
        <button type="button" className="link" onClick={() => setOpen(false)}>
          Cancel
        </button>
      </div>
    </form>
  );
}

function McpTab({ core, project, thread }: { core: Core; project: string; thread?: number }) {
  const [config, setConfig] = useState<P.McpConfig>();
  const [live, setLive] = useState<P.McpServers>();
  const [error, setError] = useState<string>();
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!project) return;
    let current = true;
    core.call("mcp/config", { project }).then(
      (c) => current && setConfig(c),
      (e: unknown) => current && setError(message(e)),
    );
    return () => {
      current = false;
    };
  }, [core, project, tick]);
  useEffect(() => {
    if (thread === undefined) return;
    let current = true;
    core.call("mcp/servers", { thread }).then(
      (s) => current && setLive(s),
      () => current && setLive(undefined),
    );
    return () => {
      current = false;
    };
  }, [core, thread, tick]);
  if (!project) return <p className="tab-body muted">Open a folder: MCP servers are configured for a project and for you.</p>;
  const write = async (scope: string, name: string, value: Record<string, unknown> | null) => {
    await core.call("mcp/config/set", { project, scope, name, value });
    setTick((t) => t + 1);
  };
  const state = (name: string) => live?.servers.find((s) => s.name === name);
  const files = config?.files ?? [];
  const exaAt = files.find((f) => f.scope === "user" && f.servers.some((s) => s.name === EXA)) ?? files.find((f) => f.servers.some((s) => s.name === EXA));
  const exa = exaAt?.servers.find((s) => s.name === EXA);
  const servers = files.flatMap((f) => f.servers.filter((s) => !(f === exaAt && s.name === EXA)).map((s) => ({ ...s, scope: f.scope, path: f.path })));
  return (
    <div className="tab-body">
      <ExaCard value={exa?.value} scope={exaAt?.scope} live={state(EXA)} onWrite={(v) => write("user", EXA, v)} />
      <h3>
        Servers <span className="muted">{servers.length}</span>
      </h3>
      {servers.length === 0 && <p className="muted">None besides Exa yet.</p>}
      {servers.map((s) => (
        <ServerRow
          key={`${s.scope}:${s.name}`}
          scope={s.scope}
          name={s.name}
          value={s.value}
          live={state(s.name)}
          project={project}
          core={core}
          onWrite={(v) => write(s.scope, s.name, v)}
        />
      ))}
      <AddServer onAdd={(scope, name, v) => write(scope, name, v)} />
      {files.filter((f) => f.problem).map((f) => (
        <p key={f.path} className="error">
          {f.path}: {f.problem}
        </p>
      ))}
      {live?.problems.map((p) => (
        <p key={p} className="warn">
          {p}
        </p>
      ))}
      {error && <p className="error">{error}</p>}
      <p className="muted small-print">
        A session starts its servers when it starts: a change here counts from the next session. A project's servers run once you trust exactly their
        commands. The files: {files.map((f) => f.path).join(", ")}.
      </p>
    </div>
  );
}

// ---------------------------------------------------------------- Instructions

type Step = AtifTrace["steps"][number];

function InstructionsTab({ core, project, thread }: { core: Core; project: string; thread?: number }) {
  const [files, setFiles] = useState<InstructionFile[]>();
  const [text, setText] = useState("");
  const [saved, setSaved] = useState<string>();
  const [error, setError] = useState<string>();
  const [system, setSystem] = useState<{ blocks: string[]; tools: string[] }>();
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let current = true;
    request("instructions/list", { project }).then(
      (f) => {
        if (!current) return;
        setFiles(f);
        const mine = f.find((x) => x.scope === "user")?.text ?? "";
        setSaved((s) => {
          if (s === undefined) setText(mine);
          return mine;
        });
      },
      (e: unknown) => current && setError(message(e)),
    );
    return () => {
      current = false;
    };
  }, [project, tick]);
  useEffect(() => {
    if (thread === undefined) return setSystem(undefined);
    let current = true;
    core.call("thread/trajectory", { thread }).then(
      (t) => {
        if (!current) return;
        const trace = t.trajectory as AtifTrace;
        const blocks = trace.steps.filter((s: Step) => s.source === "system" && s.message).map((s: Step) => s.message!);
        const tools = (trace.agent?.tool_definitions ?? []).map((d) => d.function.name);
        setSystem({ blocks, tools });
      },
      () => current && setSystem(undefined),
    );
    return () => {
      current = false;
    };
  }, [core, thread]);
  const mine = files?.find((f) => f.scope === "user");
  const dirty = saved !== undefined && text !== saved;
  const open = (path: string) =>
    request("instructions/open", { path }).then(
      () => setTick((t) => t + 1),
      (e: unknown) => setError(message(e)),
    );
  return (
    <div className="tab-body">
      <p className="muted">
        The agent's system prompt is ognivo's own for the model, then these files: yours, Claude Code's, then the project's, the later winning.
      </p>
      <h3>Your instructions, for every project</h3>
      <textarea
        className="instructions mono"
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={8}
        spellCheck={false}
        placeholder={"How you want the agent to work, in any project. For example:\n- Answer in Russian.\n- Run the tests after every change."}
      />
      <div className="buttons">
        <button
          type="button"
          className="primary small"
          disabled={!dirty || !mine}
          onClick={() =>
            mine &&
            request("instructions/save", { path: mine.path, text }).then(
              () => {
                setSaved(text);
                setError(undefined);
              },
              (e: unknown) => setError(message(e)),
            )
          }
        >
          Save
        </button>
        {mine && (
          <button type="button" className="link" title={mine.path} onClick={() => void open(mine.path)}>
            Open in the editor
          </button>
        )}
        <small className="muted">{dirty ? "not saved" : saved ? "saved" : ""}</small>
      </div>
      <h3>Files the agent reads</h3>
      <ul className="files">
        {files
          ?.filter((f) => f.scope !== "user")
          .map((f) => (
            <li key={f.path}>
              <span className="codicon codicon-file" />
              <span className="mono">{f.shown}</span>
              <span className="muted">
                {f.scope === "claude" ? "Claude Code's, yours" : f.shown === "CLAUDE.local.md" ? "the project's, yours only" : "the project's"}
                {f.exists ? ` · ${f.bytes < 1024 ? `${f.bytes} B` : `${(f.bytes / 1024).toFixed(1)} KB`}` : " · none"}
              </span>
              <span className="spacer" />
              {(f.exists || f.scope === "project") && (
                <button type="button" className="link" title={f.path} onClick={() => void open(f.path)}>
                  {f.exists ? "Open" : "Create"}
                </button>
              )}
            </li>
          ))}
      </ul>
      {error && <p className="error">{error}</p>}
      <h3>The system prompt of this session</h3>
      {thread === undefined ? (
        <p className="muted">Open a session to see what its model got, exactly.</p>
      ) : !system ? (
        <p className="muted">…</p>
      ) : (
        <>
          {system.blocks.map((b, i) => (
            <details key={i} className="prompt" open={i === 0}>
              <summary>
                {i === 0 ? "ognivo's prompt and the instructions" : "what ognivo told the model of the project"}{" "}
                <span className="muted">{b.length} characters</span>
              </summary>
              <pre>{b}</pre>
            </details>
          ))}
          {system.tools.length > 0 && (
            <p className="muted">
              Tools: <span className="mono">{system.tools.join(", ")}</span>
            </p>
          )}
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- the view

export function Settings({
  core,
  project,
  thread,
  session,
  tab,
  onTab,
  onClose,
}: {
  core: Core;
  project: string;
  /** The session on show, whose servers and prompt the tabs show. */
  thread?: number;
  session: NewSession;
  tab: SettingsTab;
  onTab: (tab: SettingsTab) => void;
  onClose: () => void;
}) {
  const tabs: [SettingsTab, string][] = [
    ["model", "Model"],
    ["mcp", "MCP"],
    ["instructions", "Instructions"],
  ];
  return (
    <div className="settings">
      <div className="tabs" role="tablist">
        {tabs.map(([id, label]) => (
          <button key={id} type="button" role="tab" aria-selected={tab === id} className={`tab ${tab === id ? "active" : ""}`} onClick={() => onTab(id)}>
            {label}
          </button>
        ))}
        <span className="spacer" />
        <button type="button" className="icon-button" title="Back to the agent" aria-label="Back to the agent" onClick={onClose}>
          <span className="codicon codicon-close" />
        </button>
      </div>
      <div className="tab-scroll">
        {tab === "model" ? (
          <ModelTab core={core} session={session} />
        ) : tab === "mcp" ? (
          <McpTab core={core} project={project} thread={thread} />
        ) : (
          <InstructionsTab core={core} project={project} thread={thread} />
        )}
      </div>
    </div>
  );
}
