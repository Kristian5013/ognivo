// The model in the composer: its pill names the model and its reasoning level, and opens a
// popover over the composer with the server and whether it answers, the models of
// llama-server's router to pick from, the reasoning level with a word on it, the context,
// and the ways to the settings. What it changes is the extension's `ognivo.*` settings,
// which new sessions start with: an open session keeps what it started with, and the
// popover says so with the session's own model, level and context.

import { useLayoutEffect, useRef, useState } from "react";

import type { NewSession, SESSION_SETTINGS, SettingsTab } from "../shared/messages.ts";
import profiles from "../shared/profiles.json";
import type * as P from "../shared/protocol.ts";
import { type Choice, type Place, Segmented, Select, placeByComposer, useAway } from "./controls.tsx";
import type { Detection } from "./detect.ts";
import { request } from "./host.ts";
import { Icon } from "./icon.tsx";
import { type Budget, byThinking, contextChoices, contextSize, levelHint, profileLevels, startLevel, windowChoice } from "./model/context.ts";

type Profile = { context: Budget; reasoning: { levels: string[]; default: string }; native: boolean };
const PROFILES = profiles as Record<string, Profile>;

/** What a router's model is now, as its row says it. */
function status(m: P.RoutedModel): string {
  const state = m.status === "unloaded" ? "loads on pick" : m.status;
  return [state, m.profile ?? "no profile"].join(" · ");
}

/** A server's address as the popover names it: no scheme, no slash at the end. */
function address(url: string): string {
  return url.trim().replace(/^https?:\/\//, "").replace(/\/+$/, "");
}

/** What an open session works with, as the core opened it. */
export interface SessionModel {
  model: string;
  level: string | null;
  context?: P.Context;
}

export function ModelPill({
  session,
  detected,
  current,
  onSettings,
}: {
  /** What a new session starts with. */
  session: NewSession;
  /** What the server of a new session runs. */
  detected: Detection;
  /** The open session's own, none before a session. */
  current?: SessionModel;
  onSettings: (tab: SettingsTab) => void;
}) {
  const [place, setPlace] = useState<Place>();
  const [error, setError] = useState<string>();
  const pill = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const open = place !== undefined;
  // what the server said last, while it is asked again about a model picked on its router:
  // the list stays, and so does the focus on the model the user picked
  const said = useRef<{ server: string; found: P.Detected }>(undefined);
  if (detected && detected !== "failed") said.current = { server: session.server, found: detected };

  const d =
    detected && detected !== "failed" ? detected : detected === undefined && said.current?.server === session.server ? said.current.found : undefined;
  const state = detected === undefined ? "pending" : detected === "failed" ? "failed" : "ok";
  const profileId = session.profile || d?.profile || "";
  const profile = PROFILES[profileId];
  const levels = byThinking(session.profile ? (profile?.reasoning.levels ?? []) : (d?.levels.length ? d.levels : (profile?.reasoning.levels ?? [])));
  const defaultLevel = (session.profile ? profile?.reasoning.default : (d?.default_level ?? profile?.reasoning.default)) ?? "";
  // the level a new session gets, as it starts with it (app.tsx): a level of another model's
  // profile is this profile's default
  const level = startLevel(session.reasoning, levels) ?? defaultLevel;
  const name = current ? current.model : session.model || d?.model || session.profile || d?.profile || "";
  const shownLevel = current ? (current.level ?? "") : level;
  const slot = d?.window ?? undefined;
  const sizes = contextChoices(profile?.context, slot);
  const whole = windowChoice(profile?.context, slot);
  const contexts: Choice<number>[] = [
    { value: 0, label: whole.charAt(0).toUpperCase() + whole.slice(1) },
    ...sizes.map((n) => ({ value: n, label: contextSize(n), hint: n === profile?.context.working_max ? "What the profile was measured at" : undefined })),
  ];

  const set = (key: (typeof SESSION_SETTINGS)[number], value: unknown) =>
    request("settings/set", { key, value }).then(
      () => setError(undefined),
      (e: unknown) => setError(e instanceof Error ? e.message : String(e)),
    );
  const show = () => setPlace(placeByComposer(pill.current!, 440, 420));
  const close = (focus = false) => {
    setPlace(undefined);
    if (focus) pill.current?.focus();
  };
  // the thread scrolls as it streams, under a popover that stays over the composer
  useAway(open, [pop, pill], () => close(), false);
  // the chosen model has the focus as it opens, else its first control
  useLayoutEffect(() => {
    if (!open) return;
    const at = pop.current?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"]') ?? pop.current?.querySelector<HTMLElement>("button");
    at?.focus();
  }, [open]);

  const models: { id: string; meta: string; checked: boolean; pick?: () => void }[] = d?.models.length
    ? d.models.map((m) => ({
        id: m.id,
        meta: status(m),
        checked: session.model ? m.id === session.model : m.id === d.model,
        pick: () => {
          void set("model", m.id);
          // a level the model's profile has not goes back to the profile's default
          if (session.reasoning && startLevel(session.reasoning, profileLevels(session.profile || m.profile)) === null) void set("reasoning", "");
        },
      }))
    : d
      ? [
          {
            id: d.model ?? d.profile ?? (session.profile || "Its model"),
            meta: [d.window ? contextSize(d.window) : "", d.native ? "Native path" : "", d.profile ? "" : "no profile"].filter(Boolean).join(" · "),
            checked: true,
          },
        ]
      : [];

  return (
    <>
      <button
        ref={pill}
        type="button"
        className="pill model"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Model: ${name || "none"}${shownLevel ? `, reasoning ${shownLevel}` : ""}`}
        title={current ? `The session's model: ${name}` : "The server and the model of a new session"}
        onClick={() => (open ? close() : show())}
      >
        <span className="pill-text">{name || "Choose a model"}</span>
        {shownLevel && <span className="pill-level">{shownLevel}</span>}
        <Icon name={open ? "chevron-up" : "chevron-down"} className="pill-chev" />
      </button>
      {place && (
        <div
          ref={pop}
          className="modelpop"
          role="dialog"
          aria-label="The model"
          style={{ left: place.left, width: place.width, top: place.top, bottom: place.bottom, maxHeight: place.maxHeight }}
          onKeyDown={(e) => {
            if (e.key === "Escape" && !e.defaultPrevented) {
              e.preventDefault();
              e.stopPropagation();
              close(true);
            }
          }}
        >
          <div className="mp-server">
            {state === "pending" ? <Icon name="loading" spin /> : <span className={`dot is-${state}`} />}
            <span className="mp-address" title={session.server}>
              {address(session.server)}
            </span>
            <span className="mp-what">
              {state === "failed" ? "does not answer" : state === "pending" ? "asking…" : d?.models.length ? "llama-server, router" : "llama-server"}
            </span>
            <span className="spacer" />
            <button type="button" className="mp-link" title="The server's address, in the settings" onClick={() => onSettings("model")}>
              Change
            </button>
          </div>
          {current && (
            <p className="mp-note">
              This session works with {current.model || "its model"}
              {current.level ? ` · ${current.level}` : ""}
              {current.context ? ` · ${contextSize(current.context.working)}` : ""}. A change here is for new sessions.
            </p>
          )}
          {models.length > 0 && (
            <div className="mp-section">
              <div className="mp-title">Model</div>
              <div
                role="radiogroup"
                aria-label="Model"
                className="mp-models"
                onKeyDown={(e) => {
                  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
                  e.preventDefault();
                  const rows = [...(e.currentTarget.querySelectorAll<HTMLButtonElement>('[role="radio"]') ?? [])];
                  const at = rows.indexOf(document.activeElement as HTMLButtonElement);
                  rows[(at + (e.key === "ArrowDown" ? 1 : rows.length - 1)) % rows.length]?.focus();
                }}
              >
                {models.map((m, i) => (
                  <button
                    key={m.id}
                    type="button"
                    role="radio"
                    aria-checked={m.checked}
                    className={`mp-model${m.checked ? " on" : ""}`}
                    // one stop for Tab: the chosen model, or the first when none is
                    tabIndex={m.checked || (i === 0 && !models.some((x) => x.checked)) ? 0 : -1}
                    onClick={m.pick}
                  >
                    <span className="mp-radio" aria-hidden />
                    <span className="mp-name">{m.id}</span>
                    <span className="mp-meta">{m.meta}</span>
                  </button>
                ))}
              </div>
            </div>
          )}
          <div className="mp-section">
            <div className="mp-title-row">
              <span className="mp-title">Reasoning</span>
              <span className="mp-for">for new sessions</span>
            </div>
            {levels.length ? (
              <>
                <Segmented className="mp-levels" label="Reasoning" value={level} choices={levels.map((l) => ({ value: l, label: l }))} onChange={(v) => void set("reasoning", v)} />
                <p className="mp-hint">{levelHint(profileId, level)}</p>
              </>
            ) : (
              <p className="mp-hint">{state === "ok" ? "This model has no reasoning levels." : "The levels come with the model."}</p>
            )}
          </div>
          <div className="mp-section mp-context">
            <span className="mp-title">Context</span>
            <Select
              label="Context"
              value={sizes.includes(session.context) ? session.context : 0}
              choices={contexts}
              onChange={(v) => void set("context", v)}
              fixed
            />
          </div>
          {error && <p className="mp-error">{error}</p>}
          <div className="mp-links">
            <button type="button" className="mp-link" onClick={() => onSettings("model")}>
              <Icon name="settings-gear" /> All settings
            </button>
            <button type="button" className="mp-link" onClick={() => onSettings("mcp")}>
              <Icon name="plug" /> MCP servers
            </button>
            <button type="button" className="mp-link" onClick={() => onSettings("instructions")}>
              <Icon name="book" /> Instructions
            </button>
          </div>
        </div>
      )}
    </>
  );
}
