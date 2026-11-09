import { type ReactNode, useLayoutEffect, useRef, useState } from "react";

/** What a window on a stream keeps of a long text: its end, from a line's start. */
const TAIL = 6000;

export function tail(text: string, max = TAIL): string {
  if (text.length <= max) return text;
  const cut = text.indexOf("\n", text.length - max);
  return `…${text.slice(cut < 0 ? text.length - max : cut)}`;
}

/** A window on what the model writes now: it follows the newest line while the user
 * leaves it at its foot, and fades out at its head once it holds more than it shows. */
export function Follow({
  content,
  label,
  className = "",
  children,
}: {
  /** What changes as the text grows. */
  content: unknown;
  label: string;
  className?: string;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const [over, setOver] = useState(false);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    if (pinned.current) el.scrollTop = el.scrollHeight;
    const more = el.scrollHeight > el.clientHeight + 1;
    setOver((was) => (was === more ? was : more));
  }, [content]);
  return (
    <div
      ref={box}
      role="log"
      aria-label={label}
      onScroll={() => {
        const el = box.current;
        if (el) pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 8;
      }}
      className={`overflow-y-auto ${over ? "live-window" : ""} ${className}`}
    >
      {children}
    </div>
  );
}
