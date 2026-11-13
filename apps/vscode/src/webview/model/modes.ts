// The modes of a thread (docs/architecture.md §8): how far its agent goes without the
// user. The composer switches them; Shift+Tab goes to the next.

import type * as P from "../../shared/protocol.ts";

export interface ModeInfo {
  mode: P.Mode;
  label: string;
  /** What it lets the agent do, as the menu says it. */
  says: string;
  /** Its codicon, in the menus and on the composer's button. */
  icon: string;
}

export const MODES: ModeInfo[] = [
  {
    mode: "manual",
    label: "Manual",
    says: "Asks before every edit, and before every command that does more than read.",
    icon: "debug-pause",
  },
  {
    mode: "edit_automatically",
    label: "Edit automatically",
    says: "Edits apply at once, to review in Changes; commands ask as usual.",
    icon: "edit",
  },
  {
    mode: "plan",
    label: "Plan",
    says: "Reads and plans, and changes nothing: you carry the plan out when it is ready.",
    icon: "lightbulb",
  },
  {
    mode: "auto",
    label: "Auto",
    says: "Edits and runs commands without asking; protected files, files outside the project and your rules still ask.",
    icon: "run-all",
  },
];

export const DEFAULT_MODE: P.Mode = "edit_automatically";

export function modeInfo(mode: P.Mode): ModeInfo {
  return MODES.find((m) => m.mode === mode) ?? MODES[1]!;
}

/** The mode Shift+Tab goes to. */
export function nextMode(mode: P.Mode): P.Mode {
  const i = MODES.findIndex((m) => m.mode === mode);
  return MODES[(i + 1) % MODES.length]!.mode;
}

export function isMode(value: unknown): value is P.Mode {
  return MODES.some((m) => m.mode === value);
}
