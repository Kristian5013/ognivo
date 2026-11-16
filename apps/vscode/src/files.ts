// The workspace's files for `@` in the message (`files/find`): VS Code's file search, without
// what `files.exclude`, `search.exclude` and each folder's .gitignore leave out, at most
// 5 000 files, listed again after 10 seconds; ranked by fuzzy.ts. The paths are as the pages
// name files (editor.ts `shownPath`): relative to the first folder, with `/`.

import { readFile } from "node:fs/promises";
import { join } from "node:path";

import * as vscode from "vscode";

import { shownPath } from "./editor.ts";
import { enabledGlobs, gitignoreGlobs, joinGlobs, rank } from "./fuzzy.ts";

/** Files listed at most. */
const CAP = 5000;
const FRESH_MS = 10_000;

async function gitignore(folder: string): Promise<string[]> {
  try {
    return gitignoreGlobs(await readFile(join(folder, ".gitignore"), "utf8"));
  } catch {
    return [];
  }
}

export class WorkspaceFiles {
  private cache?: { at: number; key: string; files: Promise<string[]> };

  /** The files that match `query`, the best first, at most `limit`; with no query, the
   * files open in tabs first. */
  async find(query: string, limit: number): Promise<string[]> {
    const files = await this.all();
    if (query.trim()) return rank(files, query, limit);
    const open = new Set<string>();
    for (const tab of vscode.window.tabGroups.all.flatMap((g) => g.tabs)) {
      if (tab.input instanceof vscode.TabInputText && tab.input.uri.scheme === "file") open.add(shownPath(tab.input.uri));
    }
    const known = new Set(files);
    const first = [...open].filter((p) => known.has(p));
    return [...first, ...rank(files, "", limit).filter((p) => !open.has(p))].slice(0, limit);
  }

  private all(): Promise<string[]> {
    const folders = (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === "file");
    const key = folders.map((f) => f.uri.toString()).join("\n");
    if (this.cache && this.cache.key === key && Date.now() - this.cache.at < FRESH_MS) return this.cache.files;
    const files = this.list(folders);
    this.cache = { at: Date.now(), key, files };
    files.catch(() => {
      if (this.cache?.files === files) this.cache = undefined;
    });
    return files;
  }

  private async list(folders: readonly vscode.WorkspaceFolder[]): Promise<string[]> {
    const out: string[] = [];
    const seen = new Set<string>();
    for (const folder of folders) {
      const exclude = joinGlobs([
        ...enabledGlobs(vscode.workspace.getConfiguration("files", folder.uri).get<Record<string, unknown>>("exclude")),
        ...enabledGlobs(vscode.workspace.getConfiguration("search", folder.uri).get<Record<string, unknown>>("exclude")),
        ...(await gitignore(folder.uri.fsPath)),
      ]);
      const uris = await vscode.workspace.findFiles(new vscode.RelativePattern(folder, "**/*"), exclude, CAP - out.length);
      for (const uri of uris) {
        const path = shownPath(uri);
        if (seen.has(path)) continue;
        seen.add(path);
        out.push(path);
      }
      if (out.length >= CAP) break;
    }
    return out;
  }
}
