import { beforeEach, describe, expect, it } from "vitest";

import type * as P from "../shared/protocol.ts";
import { Core, CoreError, type Port } from "../shared/rpc.ts";
import { detect, detectAgain, forget } from "./detect.ts";

type Request = { id: number; method: string; params: P.Server };

/** A core whose `server/detect` answers `answer` of each question, in order; and the questions. */
function core(answer: (r: Request, n: number) => Partial<P.Detected> | Error) {
  const asked: Request[] = [];
  const port: Port = {
    onmessage: null,
    postMessage(text: string) {
      const r = JSON.parse(text) as Request;
      asked.push(r);
      const a = answer(r, asked.length);
      const reply = a instanceof Error ? { error: { code: -32003, message: a.message } } : { result: a };
      queueMicrotask(() => port.onmessage?.({ data: JSON.stringify({ jsonrpc: "2.0", id: r.id, ...reply }) }));
    },
  };
  return { core: new Core(port), asked };
}

describe("what a new session's server runs", () => {
  beforeEach(() => forget());

  it("is asked once for each server and model, the parts of the page sharing the answer", async () => {
    const c = core((r) => ({ model: r.params.model }));
    const first = detect(c.core, " http://127.0.0.1:8080 ", "");
    expect(detect(c.core, "http://127.0.0.1:8080", "")).toBe(first);
    expect((await detect(c.core, "http://127.0.0.1:8080", "qwen")).model).toBe("qwen");
    expect(c.asked.map((r) => [r.method, r.params.url, r.params.model])).toEqual([
      ["server/detect", "http://127.0.0.1:8080", null],
      ["server/detect", "http://127.0.0.1:8080", "qwen"],
    ]);
  });

  it("is asked again for a server started with its projector since, and the new answer stays", async () => {
    // llama-server started again with --mmproj after the first question
    const c = core((_, n) => ({ vision: n > 1 }));
    expect((await detect(c.core, "http://127.0.0.1:8080", "")).vision).toBe(false);
    expect((await detectAgain(c.core, "http://127.0.0.1:8080", "")).vision).toBe(true);
    expect((await detect(c.core, "http://127.0.0.1:8080", "")).vision).toBe(true);
    expect(c.asked).toHaveLength(2);
    // and after the settings' Check
    forget();
    expect((await detect(c.core, "http://127.0.0.1:8080", "")).vision).toBe(true);
    expect(c.asked).toHaveLength(3);
  });

  it("is asked again next time when the server did not answer", async () => {
    const c = core((_, n) => (n === 1 ? new Error("connection refused") : { vision: true }));
    await expect(detect(c.core, "http://127.0.0.1:8080", "")).rejects.toBeInstanceOf(CoreError);
    expect((await detect(c.core, "http://127.0.0.1:8080", "")).vision).toBe(true);
    expect(c.asked).toHaveLength(2);
  });
});
