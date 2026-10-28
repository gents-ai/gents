import { useMemo, type CSSProperties } from "react";
import type { ToolDiffLineView } from "@source-inc/gents-desktop-client";
import { PatchDiff } from "@pierre/diffs/react";
import { cn } from "@gents/ui/lib/utils";
import { excerptPatch } from "@/lib/excerptPatch";
import { thinScrollbar } from "@/lib/scrollbar";
import { useTheme } from "@/preferences";
import { DiffWorkers } from "./diffWorkers";
import { optionsFor, TOKENS } from "./pierreDiff";

/* on the tool card's own surface rather than the page's */
const INLINE = { ...TOKENS, "--diff-view-bg": "transparent" } as CSSProperties;

/** A file tool's reported diff, drawn like the Changes panel's. */
export default function InlineDiff({
  path,
  lines,
}: {
  path: string;
  lines: readonly ToolDiffLineView[];
}) {
  const theme = useTheme();
  const patch = useMemo(() => excerptPatch(path, lines), [path, lines]);
  return (
    <DiffWorkers>
      <div
        style={INLINE}
        className={cn("max-h-72 overflow-y-auto rounded-md", thinScrollbar)}
      >
        <PatchDiff
          patch={patch}
          options={optionsFor(theme, lines[0]?.lineNumber ? "excerpt" : "unnumbered")}
        />
      </div>
    </DiffWorkers>
  );
}
