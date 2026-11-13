import { describe, expect, it } from "vitest";

import { DEFAULT_MODE, MODES, isMode, modeInfo, nextMode } from "./modes.ts";

describe("modes", () => {
  it("go round in the menu's order with Shift+Tab", () => {
    let mode = DEFAULT_MODE;
    const seen = [mode];
    for (let i = 0; i < MODES.length; i++) {
      mode = nextMode(mode);
      seen.push(mode);
    }
    expect(seen).toEqual(["edit_automatically", "plan", "auto", "manual", "edit_automatically"]);
  });

  it("are known by their names only", () => {
    expect(isMode("plan")).toBe(true);
    expect(isMode("yolo")).toBe(false);
    expect(modeInfo("auto").label).toBe("Auto");
  });
});
