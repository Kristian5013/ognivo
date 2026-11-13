// What the server of a new session runs, as the core finds it (`server/detect`): its model
// and profile, the profile's reasoning levels, the slot's window. One question for each
// server and model, shared by the parts of the page that show it.

import { useEffect, useState } from "react";

import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";

export type Detection = P.Detected | "failed" | undefined;

const asked = new Map<string, Promise<P.Detected>>();

/** What `server` runs (behind a router, `model`); `undefined` while the core finds out. */
export function useDetected(core: Core, server: string, model: string): Detection {
  const [found, setFound] = useState<Detection>();
  useEffect(() => {
    const url = server.trim();
    if (!url) return;
    let live = true;
    setFound(undefined);
    const key = `${url} ${model}`;
    let q = asked.get(key);
    if (!q) {
      q = core.call("server/detect", { url, api_key: null, model: model || null });
      asked.set(key, q);
      // a server that did not answer is asked again next time
      q.catch(() => asked.delete(key));
    }
    q.then(
      (d) => live && setFound(d),
      () => live && setFound("failed"),
    );
    return () => {
      live = false;
    };
  }, [core, server, model]);
  return found;
}

/** Asks again, after the server changed: a new model, a restart. */
export function forget(): void {
  asked.clear();
}
