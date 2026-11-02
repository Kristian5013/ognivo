import { describe, expect, it } from "vitest";

import { lineDelta, refusalReason } from "./format.ts";

describe("lineDelta", () => {
  it("counts the lines a change adds and removes, not the lines it keeps", () => {
    expect(lineDelta("a\nb\nc", "a\nB\nc")).toEqual({ added: 1, removed: 1 });
    expect(lineDelta("fn x() {\n    2\n}", "")).toEqual({ added: 0, removed: 3 });
    expect(lineDelta("", "one\ntwo\n")).toEqual({ added: 3, removed: 0 });
    expect(lineDelta("same", "same")).toEqual({ added: 0, removed: 0 });
  });
});

describe("refusalReason", () => {
  it("reads why a call needed an approval, or the rule that denied it", () => {
    expect(
      refusalReason(
        "[refused] cargo test needs the user's approval (it runs project code while there are unreviewed changes), and this run has no user. Do not retry it",
      ),
    ).toBe("it runs project code while there are unreviewed changes");
    expect(
      refusalReason(
        "[refused] rm -f made.txt is denied by the rule Bash(rm:*) in .ognivo/config.toml. Do not retry it; do the task another way",
      ),
    ).toBe("denied by the rule Bash(rm:*) in .ognivo/config.toml");
  });
});
