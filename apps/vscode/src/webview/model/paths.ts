// Paths as the core gives them: `/home/me/proj/src/a.rs` on Linux,
// `C:\Users\me\proj\src\a.rs` on Windows, where a model may also write `C:/Users/...`.

const WINDOWS = /^(?:[A-Za-z]:[\\/]|\\\\)/;

/** `path` relative to `project` when it lies inside it, else `path` as it is. A Windows
 * project matches either separator and any case, as its file system does. */
export function relativeTo(project: string, path: string): string {
  const root = project.replace(/[\\/]+$/, "");
  if (!root) return path;
  const windows = WINDOWS.test(project);
  const sep = path[root.length];
  if (sep !== "/" && !(windows && sep === "\\")) return path;
  const head = path.slice(0, root.length);
  const same = windows ? fold(head) === fold(root) : head === root;
  return same ? path.slice(root.length + 1) : path;
}

/** The last part of a path, after either separator. */
export function basename(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).at(-1) || path;
}

function fold(path: string): string {
  return path.replaceAll("\\", "/").toLowerCase();
}
