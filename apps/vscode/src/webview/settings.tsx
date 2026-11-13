// The panel's settings (docs/architecture.md §10), in three tabs:
// - Model: the server and what it runs, and what a new session works with (the extension's
//   `ognivo.*` settings);
// - MCP: the servers of the user, the project and Claude Code's config, with Exa ready to
//   switch on with a key; on and off, added and removed in the config files (the core
//   writes them: `mcp/config/set`); the session's servers as they run, with their sign-ins;
// - Instructions: what the agent gets besides its prompt: the user's own instructions,
//   edited here, Claude Code's and the project's files, and the exact system prompt of the
//   session on show.

import { useEffect, useState } from "react";

import type { InstructionFile, NewSession, Requests, SESSION_SETTINGS, SettingsTab } from "../shared/messages.ts";
import { entry } from "../shared/mcpform.ts";
import profiles from "../shared/profiles.json";
import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";
import { type Choice, Row, Section, Segmented, Select, Switch } from "./controls.tsx";
import { forget, useDetected } from "./detect.ts";
import { openLink, request, run } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import type { AtifTrace } from "./model/atif.ts";
import { type Budget, contextChoices, contextSize, windowChoice } from "./model/context.ts";
import { MODES } from "./model/modes.ts";

type Profile = { context: Budget; reasoning: { levels: string[]; default: string }; native: boolean };
const PROFILES = profiles as Record<string, Profile>;

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function bytes(n: number): string {
  return n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`;
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
  const check = (url: string) => {
    forget();
    void set("server", url);
  };
  const profileId = session.profile || d?.profile || "";
  const profile = PROFILES[profileId];
  const levels = session.profile ? (profile?.reasoning.levels ?? []) : (d?.levels ?? profile?.reasoning.levels ?? []);
  const defaultLevel = session.profile ? profile?.reasoning.default : (d?.default_level ?? profile?.reasoning.default);
  const window = d?.window ?? undefined;
  const choices = contextChoices(profile?.context, window);
  const native = profile?.native ?? d?.native;
  const state = detected === undefined ? "pending" : detected === "failed" ? "failed" : "ok";

  const profileChoices: Choice<string>[] = [
    { value: "", label: d?.profile ? `Auto: ${d.profile}` : "Auto", hint: "The profile of the model the server runs" },
    ...Object.keys(PROFILES).map((p) => ({ value: p, label: p })),
  ];
  const reasoningChoices: Choice<string>[] = [
    { value: "", label: "Default", hint: defaultLevel ? `The model's default: ${defaultLevel}` : "The model's default" },
    ...levels.map((l) => ({ value: l, label: l })),
  ];
  const whole = windowChoice(profile?.context, window);
  const contextChoicesShown: Choice<number>[] = [
    { value: 0, label: whole.charAt(0).toUpperCase() + whole.slice(1) },
    ...choices.map((n) => ({
      value: n,
      label: contextSize(n),
      hint: n === profile?.context.working_max ? "What the profile was measured at" : undefined,
    })),
  ];

  return (
    <div className="tab-body">
      <Section title="Server">
        <Row
          className={`server-status is-${state}`}
          icon={state === "pending" ? <Icon name="loading" spin className="srow-icon" /> : <span className={`dot is-${state}`} />}
          title={state === "failed" ? "The server does not answer" : state === "pending" ? "Asking the server…" : (d?.model ?? "Its model")}
          hint={
            state === "failed"
              ? "Is llama-server running at this address?"
              : d
                ? [d.profile ? `profile ${d.profile}` : "no profile fits it: the Generic path guesses", d.window ? `${contextSize(d.window)} context` : ""]
                    .filter(Boolean)
                    .join(" · ")
                : undefined
          }
        >
          <button type="button" className="btn ghost small" title="Ask the server again what it runs" onClick={() => check(server.trim())}>
            <Icon name="refresh" /> Check
          </button>
        </Row>
        <Row title="Address" hint="Where llama-server listens" stack>
          <input
            className="mono wide"
            aria-label="The server's address"
            value={server}
            onChange={(e) => setServer(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
            onBlur={() => {
              const url = server.trim();
              if (url && url !== session.server) check(url);
            }}
            spellCheck={false}
          />
        </Row>
        {d && d.models.length > 0 && (
          <Row className="choice" title="Model behind the router" hint="llama-server's router runs several">
            <Select
              label="Model behind the router"
              value={session.model}
              onChange={(v) => void set("model", v)}
              choices={[
                { value: "", label: "The one it runs" },
                ...d.models.map((m) => ({
                  value: m.id,
                  label: m.id,
                  hint: [m.status === "loaded" ? "loaded" : m.status, m.profile ? "" : "no profile"].filter(Boolean).join(" · "),
                })),
              ]}
            />
          </Row>
        )}
      </Section>

      <Section title="A new session">
        <Row className="choice" title="Profile" hint="How ognivo talks to the model: its format, its tools, its limits">
          <Select label="Profile" value={session.profile} choices={profileChoices} onChange={(v) => void set("profile", v)} />
        </Row>
        <Row className="choice" title="Reasoning" hint="Higher levels think longer: more time and context, not fewer mistakes of memory" stack={levels.length > 3}>
          {levels.length ? (
            <Segmented
              label="Reasoning"
              value={levels.includes(session.reasoning) ? session.reasoning : ""}
              choices={reasoningChoices}
              onChange={(v) => void set("reasoning", v)}
            />
          ) : (
            <span className="muted">none for this model</span>
          )}
        </Row>
        <Row className="choice" title="Context" hint="What the conversation may hold; it is compacted before it is full">
          <Select
            label="Context"
            value={choices.includes(session.context) ? session.context : 0}
            choices={contextChoicesShown}
            onChange={(v) => void set("context", v)}
          />
        </Row>
        <Row className="choice" title="Mode" hint={MODES.find((m) => m.mode === session.mode)?.says}>
          <Select
            label="Mode"
            value={session.mode}
            choices={MODES.map((m) => ({ value: m.mode as string, label: m.label, hint: m.says, icon: m.icon }))}
            onChange={(v) => void set("mode", v)}
          />
        </Row>
        <Row
          title="Native path"
          hint={native === false ? `ognivo renders the model's format itself; not for ${profileId} yet` : "ognivo renders the model's format itself (gpt-oss, Qwen)"}
        >
          <Switch
            label={session.native && native !== false ? "Switch the Native path off" : "Switch the Native path on"}
            on={session.native && native !== false}
            disabled={native === false}
            onChange={(on) => void set("native", on)}
          />
        </Row>
        <Row title="Subagents" hint="The agent may hand parts of a task to agents of its own (llama-server -np 2 or more)">
          <Switch label={session.subagents ? "Switch subagents off" : "Switch subagents on"} on={session.subagents} onChange={(on) => void set("subagents", on)} />
        </Row>
      </Section>

      {error && <p className="error">{error}</p>}
      <p className="tab-foot">
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
  user: "yours",
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
      ? ["ok", `${s.tools.length} tool${s.tools.length === 1 ? "" : "s"}${s.prompts.length ? `, ${s.prompts.length} prompt${s.prompts.length === 1 ? "" : "s"}` : ""}`]
      : s.state === "sign_in"
        ? ["warn", "wants a sign-in"]
        : s.state === "untrusted"
          ? ["muted", "waits for your trust"]
          : ["err", "did not start"];
  return (
    <span className={`pill-state ${cls}`} title={s.why ?? undefined}>
      <span className="pill-dot" />
      {text}
    </span>
  );
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
  const [check, setCheck] = useState<Requests["exa/check"]["result"] | "checking">();
  // `${EXA_API_KEY}`: the key is the environment's, which the core expands
  const fromEnv = /^\$\{(\w+)\}$/.exec(has)?.[1];
  const checkKey = () => {
    setCheck("checking");
    request("exa/check", { key: has }).then(setCheck, (e: unknown) => setCheck({ ok: false, says: message(e) }));
  };
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
    <>
      <Row
        className="preset"
        icon={
          <span className="tile">
            <Icon name="globe" />
          </span>
        }
        title={
          <>
            Exa <ServerState s={live} />
          </>
        }
        hint="The agent searches the web and reads pages through Exa's MCP server, with your key"
      >
        <Switch on={on} onChange={toggle} label={on ? "Switch Exa off" : "Switch Exa on"} disabled={!mine} />
      </Row>
      {!mine && <Row title={<span className="muted">Configured in {SCOPE[scope ?? ""] ?? scope} config: change it there.</span>} />}
      {mine && (asking || on || has) && (
        <div className="srow stack">
          {has && (
            <div className="exa-key">
              <Icon name="key" className="srow-icon" />
              {fromEnv ? (
                <span>
                  The key of the environment variable <code>{fromEnv}</code>
                </span>
              ) : (
                <>
                  <span>The key</span>
                  <code title="Its last four characters">••••{has.slice(-4)}</code>
                  <button type="button" className="btn ghost small" disabled={check === "checking"} onClick={checkKey}>
                    {check === "checking" ? <Icon name="loading" spin /> : <Icon name="pulse" />} Check it
                  </button>
                </>
              )}
              {check && check !== "checking" && (
                <span className={`exa-check ${check.ok ? "ok" : "err"}`}>
                  <Icon name={check.ok ? "pass-filled" : "error"} /> {check.says}
                </span>
              )}
            </div>
          )}
          <form
            className="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              if (key.trim()) void save(key.trim());
            }}
          >
            <input
              type="password"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder={has ? "A key is set: paste another to change it" : "Your Exa API key"}
              aria-label="Exa API key"
              autoFocus={asking}
              spellCheck={false}
            />
            <button type="submit" className="btn primary" disabled={!key.trim()}>
              {has ? "Change" : "Save and switch on"}
            </button>
          </form>
          <div className="srow-hint">
            <button type="button" className="link" onClick={() => openLink("https://dashboard.exa.ai/api-keys")}>
              Get a key at exa.ai
            </button>
            {" · "}runs <code>npx exa-mcp-server</code>, which needs Node.js
            {on && !live ? " · starts with the next session" : ""}
          </div>
          {error && <p className="error">{error}</p>}
        </div>
      )}
    </>
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
    <div className={`srow server${on ? "" : " off"}`}>
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
      <div className="srow-text">
        <div className="srow-title">
          <span className="server-name">{name}</span>
          <span className="tag">{SCOPE[scope] ?? scope}</span>
          <ServerState s={live} />
        </div>
        <div className="srow-hint mono server-target" title={target(value)}>
          {target(value)}
        </div>
        {error && <p className="error">{error}</p>}
      </div>
      <div className="srow-control">
        {live?.remote &&
          (live.signed_in ? (
            <button type="button" className="btn ghost small" onClick={() => void act(core.call("mcp/logout", { project, server: name }))}>
              Sign out
            </button>
          ) : (
            <button type="button" className="btn secondary small" onClick={() => void act(core.call("mcp/login", { project, server: name }))}>
              Sign in
            </button>
          ))}
        {editable &&
          (removing ? (
            <>
              <button type="button" className="btn danger small" onClick={() => void act(onWrite(null))}>
                Remove
              </button>
              <button type="button" className="btn ghost small" onClick={() => setRemoving(false)}>
                Keep
              </button>
            </>
          ) : (
            <IconButton icon="trash" title={`Remove ${name}`} onClick={() => setRemoving(true)} />
          ))}
      </div>
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
      <button type="button" className="srow add-row" onClick={() => setOpen(true)}>
        <Icon name="add" /> Add a server
      </button>
    );
  }
  return (
    <form
      className="srow stack add-form"
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
      <div className="srow-title">A new server</div>
      <label className="field">
        <span>Name</span>
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="github" autoFocus spellCheck={false} />
      </label>
      <div className="field">
        <span>For</span>
        <Segmented
          label="For"
          value={scope}
          onChange={setScope}
          choices={[
            { value: "user", label: "Me, every project", hint: "~/.ognivo/mcp.json" },
            { value: "project", label: "This project", hint: ".mcp.json" },
          ]}
        />
      </div>
      <div className="field">
        <span>Runs as</span>
        <Segmented
          label="Runs as"
          value={kind}
          onChange={setKind}
          choices={[
            { value: "command", label: "A command", icon: "terminal" },
            { value: "url", label: "A URL", icon: "globe" },
          ]}
        />
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
      <div className="form-buttons">
        <button type="button" className="btn ghost" onClick={() => setOpen(false)}>
          Cancel
        </button>
        <button type="submit" className="btn primary">
          Add
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
  if (!project) {
    return (
      <div className="tab-body">
        <p className="tab-foot">Open a folder: MCP servers are configured for a project and for you.</p>
      </div>
    );
  }
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
      <Section title="Web search">
        <ExaCard value={exa?.value} scope={exaAt?.scope} live={state(EXA)} onWrite={(v) => write("user", EXA, v)} />
      </Section>
      <Section title="Servers" aside={<span className="sec-count">{servers.length}</span>}>
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
      </Section>
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
      <p className="tab-foot">
        A session starts its servers when it starts: a change here counts from the next session. A project's servers run once you trust exactly their
        commands. The files: {files.map((f) => f.path).join(", ")}.
      </p>
    </div>
  );
}

// ---------------------------------------------------------------- Instructions

type Step = AtifTrace["steps"][number];

/** What a block of a conversation's start is: ognivo's prompt, the project's part with the
 * instruction files (ognivo-core agent.rs `system_prompt`, `project_context`), or a note
 * such as a compaction's summary. */
function part(text: string, i: number): string {
  if (text.startsWith("[ognivo] The project is at ")) return "The project, the machine and the instruction files";
  return i === 0 ? "ognivo's own prompt, the same in every session" : "ognivo's note to the model";
}

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
        // the starts of its conversations, not the notices of its turns
        const blocks = trace.steps
          .filter((s: Step) => s.source === "system" && s.message && s.extra?.conversation !== undefined)
          .map((s: Step) => s.message!);
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
  const save = () =>
    mine &&
    request("instructions/save", { path: mine.path, text }).then(
      () => {
        setSaved(text);
        setError(undefined);
      },
      (e: unknown) => setError(message(e)),
    );
  return (
    <div className="tab-body">
      <p className="tab-intro">
        The model gets ognivo's own prompt, the same in every session, then a message with the project, the machine and the files below: Claude Code's, yours,
        then the project's. Where two disagree, the later wins. A new session reads them again.
      </p>
      <Section title="Your instructions, for every project">
        <div className="editor">
          <textarea
            className="instructions"
            aria-label="Your instructions"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "s" && (e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                if (dirty) void save();
              }
            }}
            rows={8}
            spellCheck={false}
            placeholder={"How you want the agent to work, in any project. For example:\n- Answer in Russian.\n- Run the tests after every change."}
          />
          <div className="editor-bar">
            <span className="editor-path mono" title={mine?.path}>
              ~/.ognivo/AGENTS.md
            </span>
            <span className={`editor-state${dirty ? " dirty" : ""}`}>
              {dirty ? (
                "not saved"
              ) : saved ? (
                <>
                  <Icon name="check" /> Saved
                </>
              ) : (
                ""
              )}
            </span>
            <span className="spacer" />
            {mine && (
              <button type="button" className="btn ghost small" title={mine.path} onClick={() => void open(mine.path)}>
                Open in the editor
              </button>
            )}
            <button type="button" className="btn primary small" title="Save (Ctrl+S)" disabled={!dirty || !mine} onClick={() => void save()}>
              Save
            </button>
          </div>
        </div>
      </Section>
      <Section title="Files the agent reads">
        {files
          ?.filter((f) => f.scope !== "user")
          .map((f) => (
            <Row
              key={f.path}
              className={f.exists ? "file" : "file none"}
              icon={<Icon name={f.exists ? "file" : "circle-small"} className="srow-icon" />}
              title={<span className="mono">{f.shown}</span>}
              hint={`${f.scope === "claude" ? "Claude Code's, yours" : f.shown === "CLAUDE.local.md" ? "The project's, yours only" : "The project's"} · ${f.exists ? bytes(f.bytes) : "none"}`}
            >
              {(f.exists || f.scope === "project") && (
                <button type="button" className="btn ghost small" title={f.path} onClick={() => void open(f.path)}>
                  {f.exists ? "Open" : "Create"}
                </button>
              )}
            </Row>
          ))}
      </Section>
      {error && <p className="error">{error}</p>}
      <Section title="The system prompt of this session">
        {thread === undefined ? (
          <Row title={<span className="muted">Open a session to see what its model got, exactly.</span>} />
        ) : !system ? (
          <Row title={<Icon name="loading" spin />} />
        ) : (
          <>
            {system.blocks.map((b, i) => (
              <details key={i} className="prompt" open={i === 0}>
                <summary>
                  <Icon name="chevron-right" className="prompt-chev" />
                  <span>{part(b, i)}</span>
                  <span className="spacer" />
                  <span className="srow-hint">{b.length} characters</span>
                </summary>
                <pre>{b}</pre>
              </details>
            ))}
            {system.tools.length > 0 && <Row title="Tools" hint={<span className="mono">{system.tools.join(", ")}</span>} />}
          </>
        )}
      </Section>
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
  const tabs: [SettingsTab, string, string][] = [
    ["model", "Model", "server-environment"],
    ["mcp", "MCP", "plug"],
    ["instructions", "Instructions", "book"],
  ];
  return (
    <div className="settings">
      <div className="settings-head">
        <IconButton icon="arrow-left" title="Back to the agent" onClick={onClose} />
        <span className="settings-title">Settings</span>
      </div>
      <div className="tabs" role="tablist">
        {tabs.map(([id, label, icon]) => (
          <button key={id} type="button" role="tab" aria-selected={tab === id} className={`tab${tab === id ? " active" : ""}`} onClick={() => onTab(id)}>
            <Icon name={icon} />
            <span className="tab-label">{label}</span>
          </button>
        ))}
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
