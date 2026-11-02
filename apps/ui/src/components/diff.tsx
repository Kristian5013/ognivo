import { MultiFileDiff } from "@pierre/diffs/react";

const OPTIONS = {
  diffStyle: "unified",
  theme: { dark: "github-dark", light: "github-light" },
  themeType: "system",
  disableFileHeader: true,
  overflow: "wrap",
} as const;

/** What an edit replaced, as a diff of the two fragments. */
export function EditDiff({ path, before, after }: { path: string; before: string; after: string }) {
  // fragments, not files: without a last newline both sides would say they have none
  const ended = (s: string) => (s.endsWith("\n") || !s ? s : `${s}\n`);
  return (
    <div className="overflow-hidden">
      <MultiFileDiff
        oldFile={{ name: path, contents: ended(before) }}
        newFile={{ name: path, contents: ended(after) }}
        options={OPTIONS}
      />
    </div>
  );
}
