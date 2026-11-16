import { describe, expect, it } from "vitest";

import type * as P from "../shared/protocol.ts";
import { askKind, askWhy } from "./approval.tsx";

const ask = (over: Partial<P.ApprovalAsked>): P.ApprovalAsked => ({
  thread: 1,
  item: null,
  tool: "shell",
  action: "cargo test",
  reason: "it may change things the edit journal cannot undo",
  unreviewed: [],
  agent: null,
  ...over,
});
const PROJECT = "/home/u/tally";

describe("an approval's card", () => {
  it("says what the agent would do, by the tool and why the core asks", () => {
    expect(askKind(ask({}), PROJECT)).toMatchObject({ title: "Run this command?", verb: "Run", command: true });
    expect(askKind(ask({ tool: "Bash" }), PROJECT).verb).toBe("Run");
    expect(askKind(ask({ tool: "Read", action: "/etc/hosts", reason: "outside the project" }), PROJECT).title).toBe("Read a file outside the project?");
    expect(askKind(ask({ tool: "Grep", action: "/home/u", reason: "outside the project" }), PROJECT).title).toBe("Search outside the project?");
    expect(askKind(ask({ tool: "list_directory", action: "/home/u", reason: "outside the project" }), PROJECT).title).toBe("List a folder outside the project?");
    expect(askKind(ask({ tool: "mcp_call", action: "github · create_issue", reason: "an MCP tool, which does what its server does; arguments {}" }), PROJECT)).toMatchObject({
      title: "Call this MCP tool?",
      verb: "Call",
    });
    expect(askKind(ask({ tool: "Skill", action: "docs · prompt review", reason: "a prompt of an MCP server, whose arguments go to it: {}" }), PROJECT).title).toBe(
      "Use this MCP prompt?",
    );
    expect(askKind(ask({ tool: "Teleport" }), PROJECT)).toMatchObject({ title: "Allow Teleport?", verb: "Allow" });
  });

  it("tells an edit outside the project from a protected one, and one manual mode asks for", () => {
    const protectedEdit = (action: string) => askKind(ask({ tool: "Edit", action, reason: "a protected path or outside the project" }), PROJECT).title;
    expect(protectedEdit("/home/u/.cargo/config.toml")).toBe("Edit a file outside the project?");
    expect(protectedEdit("C:/Users/me/.cargo/config.toml")).toBe("Edit a file outside the project?");
    expect(protectedEdit(".github/workflows/ci.yml")).toBe("Edit a protected file?");
    expect(protectedEdit("/home/u/tally/Makefile")).toBe("Edit a protected file?");
    expect(protectedEdit("Makefile, /tmp/x")).toBe("Edit files outside the project?");
    expect(askKind(ask({ tool: "apply_patch", action: "src/a.rs, src/b.rs", reason: "manual mode: every edit asks" }), PROJECT)).toMatchObject({
      title: "Edit these files?",
      verb: "Edit",
    });
  });

  it("says why in one sentence, with the changes nobody reviewed", () => {
    expect(askWhy(ask({ reason: "outside the project" }))).toBe("The path is outside the project.");
    expect(askWhy(ask({ reason: "manual mode: every command that does more than read asks" }))).toBe("Manual mode: every command that does more than read asks.");
    expect(askWhy(ask({ reason: "it runs project code while there are unreviewed changes", unreviewed: ["src/main.rs", "README.md"] }))).toBe(
      "It runs the project's code, and 2 of ognivo's changes are not reviewed yet.",
    );
    expect(askWhy(ask({ reason: "it runs project code while there are unreviewed changes", unreviewed: ["src/main.rs"] }))).toBe(
      "It runs the project's code, and 1 of ognivo's changes is not reviewed yet.",
    );
    expect(askWhy(ask({ reason: "it may use the network", unreviewed: ["a.rs"] }))).toBe("It may use the network. Not reviewed yet: a.rs.");
  });
});
