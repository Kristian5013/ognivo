// The session's changes nobody reviewed yet, above the message, as the Changes view lists
// them (the core's edit journal): how many files and lines; each file's diff in the editor;
// accepted as they are or put back, one file or all. Putting back, and accepting all, ask
// in VS Code's dialog first (src/changes.ts).

import { useCallback, useEffect, useState, useSyncExternalStore } from "react";

import * as P from "../shared/protocol.ts";
import { type Core, CoreError } from "../shared/rpc.ts";
import { openDiff, request } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { splitPath } from "./model/diff.ts";
import type { ThreadStore } from "./model/store.ts";

function lines(f: P.ChangedFile): { added: number; removed: number } {
  return {
    added: f.hunks.reduce((n, h) => n + h.added.length, 0),
    removed: f.hunks.reduce((n, h) => n + h.removed.length, 0),
  };
}

export function ReviewBar({
  core,
  thread,
  project,
  store,
  turns,
}: {
  core: Core;
  thread: number;
  project: string;
  store?: ThreadStore;
  /** Changes as turns start and end. */
  turns: number;
}) {
  const [files, setFiles] = useState<P.ChangedFile[]>([]);
  const [open, setOpen] = useState(false);
  const [tick, setTick] = useState(0);
  const [error, setError] = useState<string>();
  const changed = useSyncExternalStore(
    useCallback((l: () => void) => store?.subscribeChanges(l) ?? (() => {}), [store]),
    () => store?.changesVersion ?? 0,
  );

  useEffect(() => {
    let live = true;
    core.call("changes/list", { thread }).then(
      (c) => live && setFiles(c.files),
      (e: unknown) => {
        // a running turn holds the journal: what it showed stays until the turn ends
        if (live && !(e instanceof CoreError && e.code === P.ERRORS.busy)) setFiles([]);
      },
    );
    return () => {
      live = false;
    };
  }, [core, thread, turns, changed, tick]);

  // the Changes view may have accepted or put back meanwhile
  useEffect(() => {
    const again = () => setTick((t) => t + 1);
    window.addEventListener("focus", again);
    return () => window.removeEventListener("focus", again);
  }, []);

  if (!files.length) return null;
  const total = files.reduce(
    (a, f) => {
      const l = lines(f);
      return { added: a.added + l.added, removed: a.removed + l.removed };
    },
    { added: 0, removed: 0 },
  );
  const act = (method: "changes/accept" | "changes/revert", path: string | null) =>
    request(method, { thread, path }).then(
      () => {
        setError(undefined);
        setTick((t) => t + 1);
      },
      (e: unknown) => setError(e instanceof Error ? e.message : String(e)),
    );
  return (
    <div className={`review${open ? " open" : ""}`}>
      <div className="review-head">
        <button type="button" className="review-toggle" aria-expanded={open} onClick={() => setOpen(!open)}>
          <Icon name={open ? "chevron-down" : "chevron-right"} className="chev" />
          <span className="review-count">
            {files.length} {files.length === 1 ? "file" : "files"} changed
          </span>
          <span className="delta">
            {total.added > 0 && <span className="plus">+{total.added}</span>}
            {total.removed > 0 && <span className="minus">−{total.removed}</span>}
          </span>
        </button>
        <span className="spacer" />
        <button type="button" className="btn ghost small" title="Put every file back as it was before the session" onClick={() => void act("changes/revert", null)}>
          Revert all
        </button>
        <button type="button" className="btn secondary small" title="Accept every change as it is" onClick={() => void act("changes/accept", null)}>
          Accept all
        </button>
      </div>
      {open && (
        <ul className="review-files">
          {files.map((f) => {
            const { name, dir } = splitPath(f.path);
            const l = lines(f);
            return (
              <li key={f.path}>
                <button type="button" className="review-file" title={`${f.path}: the change in the editor's diff`} onClick={() => openDiff(thread, f.path, project)}>
                  <Icon name={f.deleted ? "trash" : f.created ? "new-file" : "file"} className="review-icon" />
                  <span className="fname">{name}</span>
                  {dir && <span className="fdir">{dir}</span>}
                  {f.drifted && <Icon name="warning" className="warn" title="Something else changed it too since the agent's edit" />}
                  <span className="spacer" />
                  <span className="delta">
                    {l.added > 0 && <span className="plus">+{l.added}</span>}
                    {l.removed > 0 && <span className="minus">−{l.removed}</span>}
                  </span>
                </button>
                <IconButton icon="discard" title={`Revert ${f.path}…`} onClick={() => void act("changes/revert", f.path)} />
                <IconButton icon="check" title={`Accept ${f.path}`} onClick={() => void act("changes/accept", f.path)} />
              </li>
            );
          })}
        </ul>
      )}
      {error && <p className="review-error">{error}</p>}
    </div>
  );
}
