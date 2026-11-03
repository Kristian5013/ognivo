import { type ReactNode, useEffect, useState, useSyncExternalStore } from "react";

import { ACCENTS, type Accent, DEFAULT_LOOK, type Look, SCALES, look, setLook, subscribeLook } from "../look.ts";

function Choice<T extends string | number>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  label: string;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="inline-flex rounded-md border border-border p-0.5">
      {options.map((o) => (
        <button
          key={String(o.value)}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          onClick={() => onChange(o.value)}
          className={`rounded px-2.5 py-1 text-[12px] ${o.value === value ? "bg-raised text-foreground" : "text-muted-foreground hover:text-foreground"}`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 py-2">
      <span className="text-[13px]">{label}</span>
      {children}
    </div>
  );
}

/** The settings of how the GUI looks; each change shows at once. */
function Settings({ onClose }: { onClose: () => void }) {
  const l = useSyncExternalStore(subscribeLook, look);
  const set = (patch: Partial<Look>) => setLook({ ...l, ...patch });
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);
  const dark = document.documentElement.dataset.theme !== "light";
  return (
    <div className="fixed inset-0 z-20 flex items-center justify-center bg-black/45" onClick={onClose}>
      <div
        role="dialog"
        aria-label="Settings"
        className="panel-in w-[460px] rounded-xl border border-border bg-panel p-5 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="mb-2 text-[15px] font-semibold">Settings</h2>
        <Field label="Theme">
          <Choice
            label="Theme"
            value={l.theme}
            onChange={(theme) => set({ theme })}
            options={[
              { value: "system", label: "System" },
              { value: "dark", label: "Dark" },
              { value: "light", label: "Light" },
            ]}
          />
        </Field>
        <Field label="Accent">
          <div role="radiogroup" aria-label="Accent" className="flex gap-2">
            {(Object.keys(ACCENTS) as Accent[]).map((a) => (
              <button
                key={a}
                type="button"
                role="radio"
                aria-checked={a === l.accent}
                aria-label={ACCENTS[a].name}
                title={ACCENTS[a].name}
                onClick={() => set({ accent: a })}
                style={{ background: ACCENTS[a][dark ? "dark" : "light"] }}
                className={`size-5 rounded-full ring-offset-2 ring-offset-panel ${a === l.accent ? "ring-2 ring-foreground/70" : "hover:ring-2 hover:ring-border"}`}
              />
            ))}
          </div>
        </Field>
        <Field label="Scale">
          <Choice
            label="Scale"
            value={l.scale}
            onChange={(scale) => set({ scale })}
            options={SCALES.map((s) => ({ value: s, label: `${Math.round(s * 100)}%` }))}
          />
        </Field>
        <Field label="Density">
          <Choice
            label="Density"
            value={l.density}
            onChange={(density) => set({ density })}
            options={[
              { value: "comfortable", label: "Comfortable" },
              { value: "compact", label: "Compact" },
            ]}
          />
        </Field>
        <Field label="Motion">
          <Choice
            label="Motion"
            value={l.motion}
            onChange={(motion) => set({ motion })}
            options={[
              { value: "system", label: "System" },
              { value: "full", label: "Full" },
              { value: "reduced", label: "Reduced" },
            ]}
          />
        </Field>
        <footer className="mt-4 flex items-center gap-2 text-[12px]">
          <button
            type="button"
            onClick={() => setLook(DEFAULT_LOOK)}
            className="rounded border border-border px-2.5 py-1 text-muted-foreground hover:bg-raised hover:text-foreground"
          >
            Reset
          </button>
          <span className="text-muted-foreground">Kept in this window's storage.</span>
          <button
            type="button"
            onClick={onClose}
            className="ml-auto rounded border border-spark/60 px-3 py-1 hover:bg-raised"
          >
            Done
          </button>
        </footer>
      </div>
    </div>
  );
}

/** A gear that opens the settings. */
export function SettingsButton() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        type="button"
        aria-label="Settings"
        title="Settings"
        onClick={() => setOpen(true)}
        className="grid size-7 place-items-center rounded-md text-muted-foreground hover:bg-raised hover:text-foreground"
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden>
          <circle cx="12" cy="12" r="3.2" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
        </svg>
      </button>
      {open && <Settings onClose={() => setOpen(false)} />}
    </>
  );
}
