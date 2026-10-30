import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import type { TranscriptEntry } from "@/screens/transcript-groups";

/** One applied edit by a native file tool: where its recorded before and
    after copies are read from, and the size of its reported diff. */
export type FileEdit = {
  path: string;
  requestId: string;
  toolCallDocId: string;
  added: number;
  removed: number;
};

/** `added` and `removed` sum the edits' diffs, which is the file's net
    change only when it was edited once; `whole` is false when the turn
    began before the loaded rows, so earlier edits may be missing. */
export type ChangedFile = {
  path: string;
  added: number;
  removed: number;
  edits: FileEdit[];
  whole: boolean;
};

/** A file's line counts, when they are its net change in the turn. */
export const netCounts = (file: ChangedFile) =>
  file.whole && file.edits.length === 1
    ? { added: file.added, removed: file.removed }
    : null;

/** The edit `tool` applied, when it is a native file edit that confirmed
    it changed the file. Commands, plugins and previews are not edits here:
    core records the file versions only for native file tools. */
export function fileEditOf(tool: RenderedToolCallView): FileEdit | null {
  const p = tool.presentation;
  if (p.kind !== "fileEdit" || p.applied !== true || !p.path) return null;
  if (!tool.toolCallDocId || !tool.requestId) return null;
  return {
    path: p.path,
    requestId: tool.requestId,
    toolCallDocId: tool.toolCallDocId,
    added: p.diff.filter((line) => line.kind === "added").length,
    removed: p.diff.filter((line) => line.kind === "removed").length,
  };
}

/** Whether two lists name the same edits to the same files, so a turn's
    files can keep their identity while later turns stream. */
export const sameChangedFiles = (
  a: readonly ChangedFile[],
  b: readonly ChangedFile[],
) =>
  a.length === b.length &&
  a.every((file, i) => {
    const other = b[i]!;
    return (
      file.path === other.path &&
      file.added === other.added &&
      file.removed === other.removed &&
      file.whole === other.whole &&
      file.edits.length === other.edits.length &&
      file.edits.every(
        (edit, j) => edit.toolCallDocId === other.edits[j]!.toolCallDocId,
      )
    );
  });

/** Edits grouped by file, in the order each file was first changed. */
export function changedFiles(edits: readonly FileEdit[], whole = true): ChangedFile[] {
  const files = new Map<string, ChangedFile>();
  for (const edit of edits) {
    const file = files.get(edit.path) ?? {
      path: edit.path,
      added: 0,
      removed: 0,
      edits: [],
      whole,
    };
    file.added += edit.added;
    file.removed += edit.removed;
    file.edits.push(edit);
    files.set(edit.path, file);
  }
  return [...files.values()];
}

/** For each turn's final reply, by its entry key, the files the turn's
    edits changed. A turn runs from a person's message to its final reply;
    a turn still running has no final reply yet, so no entry. When
    `startLoaded` is false the rows begin partway through a turn, whose
    files are then not whole. */
export function turnChanges(
  entries: readonly TranscriptEntry[],
  finalKeys: ReadonlySet<string>,
  startLoaded = true,
): Map<string, ChangedFile[]> {
  const byFinal = new Map<string, ChangedFile[]>();
  let edits: FileEdit[] = [];
  let whole = startLoaded;
  for (const entry of entries) {
    if (entry.kind === "group") {
      for (const member of entry.members) {
        const edit = member.kind === "tool" ? fileEditOf(member.tool) : null;
        if (edit) edits.push(edit);
      }
      continue;
    }
    const kind = entry.item.kind;
    if (
      kind === "userMessage" ||
      kind === "pendingUserTurn" ||
      kind === "automatedInput"
    ) {
      edits = [];
      whole = true;
    } else if (finalKeys.has(entry.key)) {
      if (edits.length) byFinal.set(entry.key, changedFiles(edits, whole));
      edits = [];
      whole = true;
    }
  }
  return byFinal;
}

const segments = (path: string) => path.split(/[\\/]/);

/** The deepest folder holding every path, so each can be shown relative to
    it; empty when they share none. */
export function sharedFolder(paths: readonly string[]): string {
  const [first = [], ...rest] = paths.map((path) => segments(path).slice(0, -1));
  let depth = first.length;
  for (const folder of rest) {
    let i = 0;
    while (i < depth && folder[i] === first[i]) i++;
    depth = i;
  }
  return first.slice(0, depth).join("/");
}

export const relativeTo = (folder: string, path: string) =>
  folder && path.length > folder.length ? path.slice(folder.length + 1) : path;

/** The last two names of a folder, enough to recognise a project by. */
export const shortFolder = (folder: string) =>
  segments(folder).filter(Boolean).slice(-2).join("/");
