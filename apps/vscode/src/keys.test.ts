import { describe, expect, it } from "vitest";

import { KeyedLines, ServerKeys, serverRoot, withKey } from "./keys.ts";

/** VS Code's secret storage as the extension sees it: what it keeps, and its changes. */
function storage(initial?: string, failing = false) {
  let value = initial;
  const listeners: ((e: { key: string }) => void)[] = [];
  const stored: string[] = [];
  return {
    stored,
    /** Another window changes the secret. */
    elsewhere(v: string) {
      value = v;
      for (const l of listeners) l({ key: "ognivo.serverKeys" });
    },
    secrets: {
      get: (key: string) => (failing ? Promise.reject(new Error("locked")) : Promise.resolve(key === "ognivo.serverKeys" ? value : undefined)),
      store: (_key: string, v: string) => {
        value = v;
        stored.push(v);
        return Promise.resolve();
      },
      onDidChange: (l: (e: { key: string }) => void) => {
        listeners.push(l);
        return { dispose: () => listeners.splice(listeners.indexOf(l), 1) };
      },
    } as never,
  };
}

const call = (method: string, params: unknown) => JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });

describe("a server's root URL", () => {
  it("is the core's: no slash at the end and no /v1", () => {
    for (const url of ["http://h:8080", "http://h:8080/", "http://h:8080/v1", "http://h:8080/v1/", " http://h:8080 "]) {
      expect(serverRoot(url)).toBe("http://h:8080");
    }
    expect(serverRoot("https://h/llama/v1")).toBe("https://h/llama");
  });
});

describe("a call that names a server", () => {
  const keyOf = (url: string) => (serverRoot(url) === "http://h:8080" ? "sekret" : undefined);

  it("brings that server's key where it brings none", () => {
    const detect = JSON.parse(withKey(call("server/detect", { url: "http://h:8080/v1", api_key: null, model: null }), keyOf));
    expect(detect.params).toEqual({ url: "http://h:8080/v1", api_key: "sekret", model: null });
    const start = JSON.parse(withKey(call("thread/start", { project: "/p", server: { url: "http://h:8080", api_key: null, model: "m" } }), keyOf));
    expect(start.params).toEqual({ project: "/p", server: { url: "http://h:8080", api_key: "sekret", model: "m" } });
    const resume = JSON.parse(withKey(call("thread/resume", { thread: 3, server: { url: "http://h:8080", api_key: null, model: null } }), keyOf));
    expect(resume.params.server.api_key).toBe("sekret");
  });

  it("stays as it is with a key of its own, with no server named, and for another server", () => {
    for (const line of [
      call("server/detect", { url: "http://h:8080", api_key: "mine", model: null }),
      call("thread/resume", { thread: 3, server: null }),
      call("server/detect", { url: "http://other:8080", api_key: null, model: null }),
      "not JSON, \"server/detect\"",
    ]) {
      expect(withKey(line, keyOf)).toBe(line);
    }
  });

  it("is the only one read: another call stays as it is, its text naming a method or not", () => {
    const turn = call("turn/start", { thread: 1, text: 'see "thread/start" and http://h:8080', images: [] });
    expect(withKey(turn, keyOf)).toBe(turn);
  });
});

describe("the servers' keys", () => {
  it("are kept by the server's root, changed and dropped, and each change is heard", async () => {
    const s = storage();
    const keys = new ServerKeys(s.secrets);
    let heard = 0;
    keys.onDidChange(() => heard++);
    await keys.ready;
    expect(keys.loaded).toBe(true);
    await keys.set("http://h:8080/v1/", " sekret ");
    expect(keys.get("http://h:8080")).toBe("sekret");
    expect(keys.has("http://h:8080/v1")).toBe(true);
    expect(JSON.parse(s.stored.at(-1)!)).toEqual({ "http://h:8080": "sekret" });
    await keys.set("http://h:8080", "");
    expect(keys.has("http://h:8080")).toBe(false);
    expect(JSON.parse(s.stored.at(-1)!)).toEqual({});
    expect(heard).toBe(2);
  });

  it("are read as kept, what is not a key left out, and taken again as another window changes them", async () => {
    const s = storage(JSON.stringify({ "http://a:1": "ka", "http://b:2": 7 }));
    const keys = new ServerKeys(s.secrets);
    await keys.ready;
    expect([keys.get("http://a:1"), keys.has("http://b:2")]).toEqual(["ka", false]);
    const heard = new Promise<void>((resolve) => keys.onDidChange(resolve));
    s.elsewhere(JSON.stringify({ "http://b:2": "kb" }));
    await heard;
    expect([keys.has("http://a:1"), keys.get("http://b:2")]).toEqual([false, "kb"]);
  });

  it("are none where the keychain cannot be read, and the calls do not wait for it", async () => {
    const keys = new ServerKeys(storage(undefined, true).secrets);
    await keys.ready;
    expect([keys.loaded, keys.get("http://h:8080")]).toEqual([true, undefined]);
  });
});

describe("the lines to the core", () => {
  it("wait for the keys in their order, then go at once", async () => {
    let release = () => {};
    const ready = new Promise<void>((resolve) => {
      release = resolve;
    });
    const keys = { loaded: false, ready, get: (url: string) => (url === "http://h:8080" ? "sekret" : undefined) };
    const written: string[] = [];
    const lines = new KeyedLines(keys, (l) => written.push(l));
    lines.send(call("initialize", { client: "test", protocol: 1 }));
    lines.send(call("server/detect", { url: "http://h:8080", api_key: null, model: null }));
    expect(written).toEqual([]);
    keys.loaded = true;
    release();
    await ready;
    await Promise.resolve();
    // one sent now does not pass those that waited
    lines.send(call("thread/list", { project: null, limit: 1 }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(written.map((l) => JSON.parse(l).method)).toEqual(["initialize", "server/detect", "thread/list"]);
    expect(JSON.parse(written[1]!).params.api_key).toBe("sekret");
    // and once nothing waits, a line goes as it is sent
    lines.send(call("thread/list", { project: null, limit: 2 }));
    expect(written).toHaveLength(4);
  });
});
