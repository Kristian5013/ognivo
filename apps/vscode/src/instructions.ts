// The files of instructions the agent gets, as the core reads them
// (crates/ognivo-core/src/instructions.rs): the user's own, ognivo's `~/.ognivo/AGENTS.md`
// and Claude Code's `~/.claude/CLAUDE.md`, then the project's AGENTS.md, CLAUDE.md,
// .claude/CLAUDE.md and CLAUDE.local.md. The settings' Instructions tab lists them, edits
// the user's own, and opens the others in the editor.

import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

import * as vscode from "vscode";

import type { InstructionFile } from "./shared/messages.ts";
import { within } from "./shared/text.ts";
import { folders } from "./view.ts";

/** The project's files, in the order the core reads them. */
const PROJECT = ["AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"];

/** The home ognivo works in (ognivo_os::home). */
function home(env = process.env): string {
  return env.OGNIVO_HOME || homedir();
}

/** ognivo's own instructions of the user. */
export function userFile(env = process.env): string {
  return join(home(env), ".ognivo", "AGENTS.md");
}

function claudeFile(env = process.env): string {
  return join(env.CLAUDE_CONFIG_DIR || join(home(env), ".claude"), "CLAUDE.md");
}

function described(scope: InstructionFile["scope"], path: string, shown: string, withText: boolean): InstructionFile {
  const exists = existsSync(path);
  const bytes = exists ? statSync(path).size : 0;
  return {
    scope,
    path,
    shown,
    exists,
    bytes,
    text: withText ? (exists ? readFileSync(path, "utf8") : "") : undefined,
  };
}

export function list(project: string): InstructionFile[] {
  const files = [
    described("user", userFile(), "~/.ognivo/AGENTS.md", true),
    described("claude", claudeFile(), "~/.claude/CLAUDE.md", false),
  ];
  if (project) for (const name of PROJECT) files.push(described("project", join(project, name), name, false));
  return files;
}

/** A path the page may write: the user's own file, or a project file of the workspace. */
function writable(path: string): boolean {
  if (path === userFile()) return true;
  return folders().some((f) => PROJECT.some((name) => join(f, name) === path) && within(path, f));
}

export function save(path: string, text: string): void {
  if (!writable(path)) throw new Error(`${path} is not a file of instructions the panel writes`);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

/** The file in the editor; one that is not there is made empty first. */
export async function open(path: string): Promise<void> {
  if (!writable(path) && path !== claudeFile()) throw new Error(`${path} is not a file of instructions`);
  if (!existsSync(path)) save(path, "");
  await vscode.window.showTextDocument(vscode.Uri.file(path), { preview: false });
}
