import type { ToolDiffLineView } from "@source-inc/gents-desktop-client";

const SIGN = { context: " ", removed: "-", added: "+" } as const;

/** A file tool's reported lines as a one-hunk unified patch. The tool
    numbers its excerpt from a first line the old and new files share, so
    one start serves both sides; lines without numbers start at 1. */
export function excerptPatch(path: string, lines: readonly ToolDiffLineView[]): string {
  const start = lines[0]?.lineNumber ?? 1;
  const side = (count: number) => `${count ? start : start - 1},${count}`;
  const old = lines.filter((line) => line.kind !== "added").length;
  const now = lines.filter((line) => line.kind !== "removed").length;
  return [
    `--- a/${path}`,
    `+++ b/${path}`,
    `@@ -${side(old)} +${side(now)} @@`,
    ...lines.map((line) => SIGN[line.kind] + line.text),
    "",
  ].join("\n");
}
