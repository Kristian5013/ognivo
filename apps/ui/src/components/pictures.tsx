import { useEffect, useState } from "react";

import { describe } from "../images.ts";
import { type Picture, pictureUrl } from "../model.ts";

/** An image whole, over the window; a click or Escape closes it. */
function Lightbox({ picture, label, onClose }: { picture: Picture; label: string; onClose: () => void }) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, [onClose]);
  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={label}
      onClick={onClose}
      className="fixed inset-0 z-50 flex cursor-zoom-out items-center justify-center bg-black/75 p-6"
    >
      <figure className="flex max-h-full max-w-full flex-col items-center gap-2">
        <img
          src={pictureUrl(picture)}
          alt={label}
          className="max-h-[85vh] min-h-48 max-w-[90vw] rounded border border-border bg-panel object-contain"
        />
        <figcaption className="text-[12px] text-white/80">
          {picture.width}×{picture.height} · Escape closes
        </figcaption>
      </figure>
    </div>
  );
}

/** Images as a row of thumbnails, each opening whole; `onRemove` gives each a button that
 * takes it away. */
export function Pictures({
  pictures,
  label,
  size = "h-24",
  onRemove,
}: {
  pictures: (Picture & { name?: string; bytes?: number })[];
  /** What the images are, for each one's name: `Image 2 of your message`. */
  label: string;
  size?: string;
  onRemove?: (index: number) => void;
}) {
  const [shown, setShown] = useState<number>();
  if (pictures.length === 0) return null;
  const nameOf = (i: number) => `Image ${i + 1} ${label}`;
  return (
    <div className="flex flex-wrap gap-2">
      {pictures.map((p, i) => (
        <figure key={i} className="group/pic relative">
          <button
            type="button"
            onClick={() => setShown(i)}
            title={p.bytes === undefined ? `${p.width}×${p.height}` : `${p.name ?? ""} · ${describe({ ...p, bytes: p.bytes })}`}
            className="block cursor-zoom-in overflow-hidden rounded border border-border bg-panel"
          >
            <img src={pictureUrl(p)} alt={nameOf(i)} className={`${size} w-auto max-w-56 object-cover`} />
          </button>
          {onRemove && (
            <button
              type="button"
              aria-label={`Remove ${nameOf(i).toLowerCase()}`}
              onClick={() => onRemove(i)}
              className="absolute -top-1.5 -right-1.5 flex size-5 items-center justify-center rounded-full border border-border bg-raised text-[11px] leading-none text-muted-foreground hover:text-foreground"
            >
              ×
            </button>
          )}
        </figure>
      ))}
      {shown !== undefined && pictures[shown] && (
        <Lightbox picture={pictures[shown]} label={nameOf(shown)} onClose={() => setShown(undefined)} />
      )}
    </div>
  );
}
