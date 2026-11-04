import { useCallback, useRef, useState } from "react";

/** A panel's width as the user dragged it, kept in the window's storage. */
export function useWidth(key: string, initial: number, min: number, max: number): [number, (w: number) => void] {
  const name = `ognivo.width.${key}`;
  const [width, setWidth] = useState(() => {
    try {
      const v = Number(localStorage.getItem(name));
      return v >= min && v <= max ? v : initial;
    } catch {
      return initial;
    }
  });
  const set = useCallback(
    (w: number) => {
      const v = Math.round(Math.min(max, Math.max(min, w)));
      setWidth(v);
      try {
        localStorage.setItem(name, String(v));
      } catch {
        // a private window: the width lasts as long as the page
      }
    },
    [name, min, max],
  );
  return [width, set];
}

/** The edge of a panel on the `side` of the window, to drag; a double click puts the panel
 * back to its first width, and the arrow keys move the edge too. */
export function Resizer({
  width,
  onWidth,
  side,
  initial,
}: {
  width: number;
  onWidth: (w: number) => void;
  side: "left" | "right";
  initial: number;
}) {
  const from = useRef<{ x: number; width: number }>(undefined);
  const grow = side === "left" ? 1 : -1;
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the panel"
      aria-valuenow={width}
      tabIndex={0}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        from.current = { x: e.clientX, width };
      }}
      onPointerMove={(e) => {
        const f = from.current;
        if (f) onWidth(f.width + grow * (e.clientX - f.x));
      }}
      onPointerUp={(e) => {
        from.current = undefined;
        e.currentTarget.releasePointerCapture(e.pointerId);
      }}
      onDoubleClick={() => onWidth(initial)}
      onKeyDown={(e) => {
        if (e.key === "ArrowLeft") onWidth(width - grow * 16);
        if (e.key === "ArrowRight") onWidth(width + grow * 16);
      }}
      className="group/edge relative z-10 w-px shrink-0 cursor-col-resize bg-border outline-none hover:bg-spark/60 focus-visible:bg-spark"
    >
      {/* a wider place to take hold of */}
      <div className="absolute inset-y-0 -left-1.5 -right-1.5" />
    </div>
  );
}
