import { parseDiffFromFile } from "@pierre/diffs";
import type { ToolDiffLineView } from "@source-inc/gents-desktop-client";

type Hunk = { header: string; lines: ToolDiffLineView[] };

const line = (kind: ToolDiffLineView["kind"], text: string): ToolDiffLineView => ({
  kind,
  text: text.replace(/\r?\n$/, ""),
});

/** The changed stretches between two versions, each with a little context
    and its unified hunk header. */
function hunksOf(before: string | null, after: string | null): Hunk[] {
  const name = "file";
  const diff = parseDiffFromFile(
    before === null ? null : { name, contents: before },
    after === null ? null : { name, contents: after },
  );
  return diff.hunks.map((hunk) => {
    const lines: ToolDiffLineView[] = [];
    for (const part of hunk.hunkContent) {
      if (part.type === "context") {
        for (let i = 0; i < part.lines; i++)
          lines.push(
            line("context", diff.additionLines[part.additionLineIndex + i] ?? ""),
          );
        continue;
      }
      for (let i = 0; i < part.deletions; i++)
        lines.push(
          line("removed", diff.deletionLines[part.deletionLineIndex + i] ?? ""),
        );
      for (let i = 0; i < part.additions; i++)
        lines.push(line("added", diff.additionLines[part.additionLineIndex + i] ?? ""));
    }
    return {
      header: `@@ -${hunk.deletionStart},${hunk.deletionCount} +${hunk.additionStart},${hunk.additionCount} @@`,
      lines,
    };
  });
}

const SIGN = { context: " ", removed: "-", added: "+" } as const;

/** The two versions as a unified patch of `path`, to copy or apply. */
export function patchText(path: string, before: string | null, after: string | null) {
  const out = [
    before === null ? "--- /dev/null" : `--- a/${path}`,
    after === null ? "+++ /dev/null" : `+++ b/${path}`,
  ];
  for (const hunk of hunksOf(before, after))
    out.push(hunk.header, ...hunk.lines.map((l) => SIGN[l.kind] + l.text));
  return out.join("\n") + "\n";
}
