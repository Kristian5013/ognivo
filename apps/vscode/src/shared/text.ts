// Small words the extension and its pages share.

/** A path as one spelling for comparing: `/` for `\`, no slash at the end, one case where
 * the path is Windows'. */
export function samePath(path: string): string {
  const p = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[A-Za-z]:\//.test(p) ? p.toLowerCase() : p;
}

/** Whether `path` is `root` or inside it. */
export function within(path: string, root: string): boolean {
  const p = samePath(path);
  const r = samePath(root);
  return p === r || p.startsWith(`${r}/`);
}

/** How long ago `unix` (seconds) was, in words. */
export function ago(unix: number, now = Date.now()): string {
  const s = Math.max(0, Math.round(now / 1000 - unix));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  const d = Math.round(s / 86400);
  return d === 1 ? "yesterday" : `${d} days ago`;
}
