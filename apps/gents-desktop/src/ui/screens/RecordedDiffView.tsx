import { useMemo } from "react";
import type { FileContents } from "@pierre/diffs";
import { MultiFileDiff } from "@pierre/diffs/react";
import {
  recordedKey,
  retryRecordedDiffs,
  useRecordedDiff,
} from "../../hooks/fileChanges";
import type { RecordedDiff, SessionScope } from "../../lib/recordedFiles";
import type { ChangedFile } from "@/lib/turnChanges";
import { useTheme } from "@/preferences";
import { DiffWorkers } from "./diffWorkers";
import { optionsFor, TOKENS } from "./pierreDiff";

/* The versions as the diff view takes them, kept while the read is: the
   view parses and draws the diff again whenever it is handed new objects. */
type Versions =
  | { oldFile: FileContents; newFile: FileContents }
  | { oldFile: null; newFile: FileContents }
  | { oldFile: FileContents; newFile: null };

function versionsOf(key: string, path: string, diff: RecordedDiff): Versions | null {
  if (diff.kind !== "text") return null;
  const side = (contents: string | null, name: string) =>
    contents === null ? null : { name: path, contents, cacheKey: `${key}\0${name}` };
  const oldFile = side(diff.before, "before");
  const newFile = side(diff.after, "after");
  if (oldFile && newFile) return { oldFile, newFile };
  if (newFile) return { oldFile: null, newFile };
  if (oldFile) return { oldFile, newFile: null };
  return null;
}

export default function RecordedDiffView({
  scope,
  file,
  whole = false,
}: {
  scope: SessionScope;
  file: ChangedFile;
  /** show the unchanged lines too, not only those around each change */
  whole?: boolean;
}) {
  const diff = useRecordedDiff(scope, file);
  const key = recordedKey(scope, file);
  const versions = useMemo(
    () => versionsOf(key, file.path, diff),
    [key, file.path, diff],
  );
  const theme = useTheme();
  if (diff.kind === "unavailable")
    return (
      <p className="px-3 py-3 text-sm text-muted-foreground">
        {diff.reason}{" "}
        <button
          type="button"
          className="underline underline-offset-2 outline-none hover:text-foreground focus-visible:text-foreground"
          onClick={retryRecordedDiffs}
        >
          Try again
        </button>
      </p>
    );
  if (diff.before === null && diff.after === null)
    return (
      <p className="px-3 py-3 text-sm text-muted-foreground">
        This file was not changed.
      </p>
    );
  const options = optionsFor(theme, whole ? "whole-file" : "file");
  return (
    <DiffWorkers>
      <div style={TOKENS}>
        {diff.partial && (
          <p className="px-3 pt-3 text-xs text-muted-foreground">
            Something else changed this file between edits, so this shows the last edit.
          </p>
        )}
        <MultiFileDiff {...versions!} options={options} />
      </div>
    </DiffWorkers>
  );
}
