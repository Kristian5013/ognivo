// The settings' controls, drawn in the panel's own way rather than the browser's: groups of
// rows (what a setting is on the left, its control on the right, one under the other in a
// narrow side bar), a switch, a choice among a few as segments, and a menu of choices that
// opens over the page where there is room. Where a menu opens, and what closes it, serve
// the composer's popovers too (the model's, the notes').

import { type ReactNode, type RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";

import { Icon } from "./icon.tsx";

/** A titled group of rows. */
export function Section({ title, aside, children }: { title: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <section className="sec">
      <div className="sec-head">
        <h3 className="sec-title">{title}</h3>
        {aside}
      </div>
      <div className="group">{children}</div>
    </section>
  );
}

/** A setting: what it is, a word on it, and its control. */
export function Row({
  title,
  hint,
  icon,
  children,
  stack,
  className,
}: {
  title: ReactNode;
  hint?: ReactNode;
  icon?: ReactNode;
  children?: ReactNode;
  /** The control under the words, whatever the width: a text field. */
  stack?: boolean;
  className?: string;
}) {
  return (
    <div className={`srow${stack ? " stack" : ""}${className ? ` ${className}` : ""}`}>
      {icon}
      <div className="srow-text">
        <div className="srow-title">{title}</div>
        {hint && <div className="srow-hint">{hint}</div>}
      </div>
      {children !== undefined && children !== null && children !== false && <div className="srow-control">{children}</div>}
    </div>
  );
}

/** On and off. */
export function Switch({ on, onChange, label, disabled }: { on: boolean; onChange: (on: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      title={label}
      disabled={disabled}
      className={`switch${on ? " on" : ""}`}
      onClick={() => onChange(!on)}
    >
      <span />
    </button>
  );
}

export interface Choice<T> {
  value: T;
  label: string;
  /** A word on it, under its label in a menu, as a tooltip on a segment. */
  hint?: string;
  icon?: string;
}

/** One of a few, side by side. */
export function Segmented<T extends string | number>({
  value,
  choices,
  onChange,
  label,
  className,
}: {
  value: T;
  choices: Choice<T>[];
  onChange: (value: T) => void;
  label: string;
  className?: string;
}) {
  return (
    <div className={`seg${className ? ` ${className}` : ""}`} role="radiogroup" aria-label={label}>
      {choices.map((c) => (
        <button
          key={String(c.value)}
          type="button"
          role="radio"
          aria-checked={c.value === value}
          className={c.value === value ? "on" : undefined}
          title={c.hint}
          onClick={() => onChange(c.value)}
        >
          {c.icon && <Icon name={c.icon} />}
          {c.label}
        </button>
      ))}
    </div>
  );
}

/** Where a menu opens: under its button, or over it when the room is above. */
export interface Place {
  left: number;
  width: number;
  top?: number;
  bottom?: number;
  maxHeight: number;
}

/** Where a menu about `tall` pixels high opens by a button at `r`: under it if it fits there
 * or the room is there, else over it, `gap` pixels away (4); `width` wide (at least the
 * button's and 240 when not given), at the button's right edge, or its left with `align:
 * "left"`, and inside the window by `margin`. */
export function placeBy(r: DOMRect, tall: number, opts: { width?: number; align?: "left" | "right"; margin?: number; gap?: number } = {}): Place {
  const margin = opts.margin ?? 8;
  const gap = opts.gap ?? 4;
  const below = window.innerHeight - r.bottom - margin;
  const above = r.top - margin;
  const width = Math.min(opts.width ?? Math.max(r.width, 240), window.innerWidth - 2 * margin);
  const at = opts.align === "left" ? r.left : r.right - width;
  const left = Math.max(margin, Math.min(at, window.innerWidth - width - margin));
  return below >= tall || below >= above
    ? { left, width, top: r.bottom + gap, maxHeight: below }
    : { left, width, bottom: window.innerHeight - r.top + gap, maxHeight: above };
}

/** Where a popover of the composer's opens: by the composer around `el`, above what is
 * written in it rather than over it, from its left edge, as wide as it up to `widest`. */
export function placeByComposer(el: Element, tall: number, widest: number): Place {
  const r = (el.closest(".composer") ?? el).getBoundingClientRect();
  return placeBy(r, tall, { width: Math.min(r.width, widest), align: "left", margin: 12, gap: 8 });
}

/** While `open`, a press outside `inside` closes it, and so does a resize of the window;
 * with `scroll`, a scroll of the page outside it too, which would leave it floating away
 * from its button. */
export function useAway(open: boolean, inside: RefObject<HTMLElement | null>[], close: () => void, scroll = true): void {
  const latest = useRef(close);
  latest.current = close;
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!inside.some((r) => r.current?.contains(t))) latest.current();
    };
    const gone = () => latest.current();
    // its own list scrolls too, to the chosen one as it opens: that keeps it open (a menu
    // of modes with Auto, its last, chosen closed as it opened, 2026-11-13)
    const scrolled = (e: Event) => {
      if (e.target instanceof Node && inside.some((r) => r.current?.contains(e.target as Node))) return;
      latest.current();
    };
    window.addEventListener("mousedown", away);
    window.addEventListener("resize", gone);
    if (scroll) window.addEventListener("scroll", scrolled, true);
    return () => {
      window.removeEventListener("mousedown", away);
      window.removeEventListener("resize", gone);
      window.removeEventListener("scroll", scrolled, true);
    };
    // the refs are the component's own: the same objects each render
  }, [open, scroll]);
}

/** One of many, in a menu. */
export function Select<T extends string | number>({
  value,
  choices,
  onChange,
  label,
  disabled,
  fixed,
}: {
  value: T;
  choices: Choice<T>[];
  onChange: (value: T) => void;
  label: string;
  disabled?: boolean;
  /** Its button stays where it is while the page scrolls (a popover's over the composer,
   * as the thread streams under it): the menu stays open then. */
  fixed?: boolean;
}) {
  const [place, setPlace] = useState<Place>();
  const button = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const current = choices.find((c) => c.value === value) ?? choices[0];
  const open = place !== undefined;

  const show = () => {
    // about as tall as it will be: under the button if it fits there, else where more room is
    const tall = Math.min(420, choices.reduce((h, c) => h + (c.hint ? 66 : 32), 10));
    setPlace(placeBy(button.current!.getBoundingClientRect(), tall));
  };
  const close = (focus = false) => {
    setPlace(undefined);
    if (focus) button.current?.focus();
  };
  // the page scrolls under it: it closes rather than float away from its button
  useAway(open, [menu, button], () => close(), !fixed);

  // the chosen one has the focus as the menu opens
  useLayoutEffect(() => {
    if (open) menu.current?.querySelector<HTMLButtonElement>('[aria-selected="true"]')?.focus();
  }, [open]);

  return (
    <>
      <button
        ref={button}
        type="button"
        className="select"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`${label}: ${current?.label ?? ""}`}
        disabled={disabled}
        onClick={() => (open ? close() : show())}
        onKeyDown={(e) => {
          if ((e.key === "ArrowDown" || e.key === "ArrowUp") && !open) {
            e.preventDefault();
            show();
          }
        }}
      >
        {current?.icon && <Icon name={current.icon} className="select-icon" />}
        <span className="select-text">{current?.label}</span>
        <Icon name="chevron-down" className="select-chev" />
      </button>
      {place && (
        <div
          ref={menu}
          className="menu floating"
          role="listbox"
          aria-label={label}
          style={{ left: place.left, width: place.width, top: place.top, bottom: place.bottom, maxHeight: place.maxHeight }}
          onKeyDown={(e) => {
            const items = [...(menu.current?.querySelectorAll<HTMLButtonElement>("[role=option]") ?? [])];
            const at = items.indexOf(document.activeElement as HTMLButtonElement);
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              items[(at + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
            } else if (e.key === "Escape" || e.key === "Tab") {
              e.preventDefault();
              close(true);
            }
          }}
        >
          {choices.map((c) => (
            <button
              key={String(c.value)}
              type="button"
              role="option"
              aria-selected={c.value === value}
              className={`menu-item${c.value === value ? " current" : ""}`}
              onClick={() => {
                onChange(c.value);
                close(true);
              }}
            >
              {c.icon && <Icon name={c.icon} className="menu-icon" />}
              <span className="menu-text">
                <span className="menu-label">{c.label}</span>
                {c.hint && <span className="menu-says">{c.hint}</span>}
              </span>
              {c.value === value && <Icon name="check" className="menu-check" />}
            </button>
          ))}
        </div>
      )}
    </>
  );
}
