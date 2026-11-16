// What goes with a message besides its words, as chips above the composer's text: the file
// the user works in or the selection in it (by itself, "current file"), a command that
// failed in the user's terminal, a problem, an image. A chip goes with the message until
// the user takes it away; a failed command the user did not attach yet is offered as a
// dashed chip, which a click attaches. The chips' text is what the message's context block
// says of them (src/shared/context.ts), so a sent message shows them the same.

import { chipLabel } from "../shared/context.ts";
import type { ContextChip } from "../shared/messages.ts";
import { Icon } from "./icon.tsx";
import { type Attachment, describe } from "./model/images.ts";
import { pictureUrl } from "./model/model.ts";

/** A chip's codicon, and the tone it is drawn in. */
function look(c: ContextChip): { icon: string; tone: string } {
  switch (c.kind) {
    case "file":
      return { icon: "file", tone: "file" };
    case "selection":
      return { icon: "list-selection", tone: "selection" };
    case "terminal":
      return { icon: "terminal", tone: "terminal" };
    case "problem":
      return { icon: c.severity === "error" ? "error" : c.severity === "warning" ? "warning" : "info", tone: c.severity };
  }
}

/** All a chip says, on hover. */
function detail(c: ContextChip): string {
  switch (c.kind) {
    case "file":
      return c.path;
    case "selection":
      return `${c.path}, ${c.start === c.end ? `line ${c.start}` : `lines ${c.start}–${c.end}`}`;
    case "terminal":
      return `${c.command || "A command"} exited with ${c.exitCode}${c.cwd ? ` in ${c.cwd}` : ""}`;
    case "problem":
      return `${c.path}, line ${c.line}: ${c.message}`;
  }
}

/** A chip of the message being written; `hint` after its label, muted. */
export function Chip({ chip, hint, onRemove }: { chip: ContextChip; hint?: string; onRemove?: () => void }) {
  const label = chipLabel(chip);
  const { icon, tone } = look(chip);
  return (
    <span className={`chip ctx-chip is-${tone}`} title={detail(chip)}>
      <Icon name={icon} className="ctx-chip-icon" />
      <span className="chip-label">{label}</span>
      {hint && <span className="ctx-chip-hint">{hint}</span>}
      {onRemove && (
        <button type="button" className="ctx-chip-x" aria-label={`Remove ${label}`} title="Not with this message" onClick={onRemove}>
          <Icon name="close" />
        </button>
      )}
    </span>
  );
}

/** An image of the message being written. */
export function ImageChip({ image, index, onRemove }: { image: Attachment; index: number; onRemove: () => void }) {
  const label = `image ${index + 1}`;
  return (
    <span className="chip ctx-chip is-image" title={`${image.name}: ${describe(image)}`}>
      <img className="ctx-chip-thumb" src={pictureUrl(image)} alt="" />
      <span className="chip-label">{label}</span>
      <button type="button" className="ctx-chip-x" aria-label={`Remove ${label}`} title="Not with this message" onClick={onRemove}>
        <Icon name="close" />
      </button>
    </span>
  );
}

/** A chip offered, not attached: dashed, with a +. */
export function SuggestChip({ chip, onAttach, onDismiss }: { chip: ContextChip; onAttach: () => void; onDismiss: () => void }) {
  const label = chipLabel(chip);
  return (
    <span className="chip ctx-chip suggest" title={`${detail(chip)}: attach what it wrote`}>
      <button type="button" className="ctx-chip-add" aria-label={`Attach ${label}`} onClick={onAttach}>
        <Icon name="add" className="ctx-chip-icon" />
        <span className="chip-label">{label}</span>
      </button>
      <button type="button" className="ctx-chip-x" aria-label={`Do not offer ${label}`} title="Do not offer it" onClick={onDismiss}>
        <Icon name="close" />
      </button>
    </span>
  );
}
