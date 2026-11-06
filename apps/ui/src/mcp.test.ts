import { describe, expect, it } from "vitest";

import { entry, pairs, words } from "./components/mcp.tsx";

describe("the MCP panel's form", () => {
  it("splits a command line as a shell does", () => {
    expect(words(`npx -y "@scope/server one" '--dir=a b'  x`)).toEqual(["npx", "-y", "@scope/server one", "--dir=a b", "x"]);
    expect(words(`a "" b`)).toEqual(["a", "", "b"]);
  });

  it("reads environment and header lines", () => {
    expect(pairs("A=1\n\n B = two \n", "=")).toEqual({ A: "1", B: "two" });
    expect(pairs("Authorization: Bearer ${TOKEN}", ":")).toEqual({ Authorization: "Bearer ${TOKEN}" });
    expect(pairs("no separator", "=")).toBe("not KEY=value: no separator");
  });

  it("makes the entry a config file holds", () => {
    expect(entry("command", "python3 -I echo.py", "")).toEqual({ command: "python3", args: ["-I", "echo.py"] });
    expect(entry("command", "node s.js", "K=v")).toEqual({ command: "node", args: ["s.js"], env: { K: "v" } });
    expect(entry("url", " https://x.dev/mcp ", "X-Key: 1")).toEqual({ type: "http", url: "https://x.dev/mcp", headers: { "X-Key": "1" } });
    expect(entry("url", "x.dev/mcp", "")).toBe("a URL of http:// or https://");
    expect(entry("command", "  ", "")).toBe("a command to start the server");
  });
});
