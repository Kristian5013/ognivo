// What the server of a new session runs, as the core finds it (`server/detect`): its model
// and profile, the profile's reasoning levels, the slot's window, whether the model takes
// images. One question for each server and model, shared by the parts of the page that show
// it; asked again after the server may have changed (`forget`, `detectAgain`), and every part
// of the page takes the new answer.

import { useEffect, useState, useSyncExternalStore } from "react";

import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";

export type Detection = P.Detected | "failed" | undefined;

const asked = new Map<string, Promise<P.Detected>>();
/** Changes each time the questions are asked anew. */
let version = 0;
const listeners = new Set<() => void>();

function changed(): void {
  version++;
  for (const l of listeners) l();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The question to `server` (behind a router, of `model`): the one asked already, or a new one. */
export function detect(core: Core, server: string, model: string): Promise<P.Detected> {
  const url = server.trim();
  const key = `${url} ${model}`;
  const have = asked.get(key);
  if (have) return have;
  const q = core.call("server/detect", { url, api_key: null, model: model || null });
  asked.set(key, q);
  // a server that did not answer is asked again next time
  q.catch(() => {
    if (asked.get(key) === q) asked.delete(key);
  });
  return q;
}

/** What `server` runs (behind a router, `model`); `undefined` while the core finds out. Asked
 * again, it says the last answer until the new one comes. */
export function useDetected(core: Core, server: string, model: string): Detection {
  const [found, setFound] = useState<{ key: string; detection: Detection }>();
  const asking = useSyncExternalStore(subscribe, () => version);
  const url = server.trim();
  const key = `${url} ${model}`;
  useEffect(() => {
    if (!url) return;
    let live = true;
    detect(core, url, model).then(
      (d) => live && setFound({ key, detection: d }),
      () => live && setFound({ key, detection: "failed" }),
    );
    return () => {
      live = false;
    };
  }, [core, url, model, key, asking]);
  return found?.key === key ? found.detection : undefined;
}

/** Asks again, after the server changed: a new model, a restart. */
export function forget(): void {
  asked.clear();
  changed();
}

/** Asks `server` again what it runs (behind a router, `model`), for every part of the page
 * that shows it: it may have been started with its projector since, or without it. */
export function detectAgain(core: Core, server: string, model: string): Promise<P.Detected> {
  asked.delete(`${server.trim()} ${model}`);
  const q = detect(core, server, model);
  changed();
  return q;
}
