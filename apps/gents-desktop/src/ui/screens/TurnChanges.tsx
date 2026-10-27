import { cn } from "@gents/ui/lib/utils";
import { useDock } from "@/app/workspace";
import { CARD_FILES } from "@/lib/rowHeights";
import type { SessionScope } from "../../lib/recordedFiles";
import {
  relativeTo,
  sharedFolder,
  shortFolder,
  type ChangedFile,
} from "@/lib/turnChanges";
import { changesView, useChosenPath } from "./changes";

/* The card stays small however many files a turn changed, and its height
   is what the transcript guesses for a row not yet drawn; the Changes
   panel lists every file. */
const SHOWN = CARD_FILES;

export const fileName = (path: string) => path.split(/[\\/]/).pop() || path;
export const folderOf = (path: string) => path.slice(0, -fileName(path).length - 1);

export function Counts({ added, removed }: { added: number; removed: number }) {
  return (
    <span className="shrink-0 font-mono text-xs tabular-nums">
      <span className="text-diff-added-fg">+{added}</span>{" "}
      <span className="text-diff-removed-fg">−{removed}</span>
    </span>
  );
}

/** A file by name, then its folder below `within`, the folder its turn's
    files share; the whole path shows on hover. */
export function FileLabel({ path, within }: { path: string; within: string }) {
  const folder = folderOf(relativeTo(within, path));
  return (
    <span className="flex min-w-0 items-baseline gap-1.5" title={path}>
      <span className="min-w-0 truncate">{fileName(path)}</span>
      {folder && (
        <span className="min-w-0 shrink-[4] truncate text-xs text-muted-foreground">
          {folder}
        </span>
      )}
    </span>
  );
}

/** A changed file as a row to choose, marked when it is the chosen one. */
export function FileRow({
  file,
  within,
  chosen,
  onChoose,
}: {
  file: ChangedFile;
  within: string;
  chosen: boolean;
  onChoose: () => void;
}) {
  return (
    <button
      type="button"
      aria-current={chosen || undefined}
      onClick={onChoose}
      className={cn(
        "flex w-full items-center justify-between gap-3 px-3 py-1 text-left outline-none hover:bg-accent focus-visible:bg-accent",
        chosen && "bg-accent",
      )}
    >
      <FileLabel path={file.path} within={within} />
      <Counts added={file.added} removed={file.removed} />
    </button>
  );
}

/** The files a turn's edits changed, under its final reply; a file opens
    its recorded versions in the Changes panel. */
export function TurnChanges({
  scope,
  turn,
  files,
}: {
  scope: SessionScope;
  turn: string;
  files: readonly ChangedFile[];
}) {
  const { openSurface } = useDock();
  const chosen = useChosenPath(scope.sessionId, turn);
  const listed = files.slice(0, SHOWN);
  const added = files.reduce((n, f) => n + f.added, 0);
  const removed = files.reduce((n, f) => n + f.removed, 0);
  const within = sharedFolder(files.map((f) => f.path));
  const open = (path: string) => {
    changesView.show({ scope, turn, files, path });
    openSurface("changes");
  };
  return (
    <section
      aria-label="Changed files"
      className="mt-2 rounded-lg bg-accent/40 py-1.5 text-sm"
      data-testid="turn-changes"
    >
      <header className="flex items-center justify-between gap-2 px-3 py-1 text-xs text-muted-foreground">
        <span className="min-w-0 truncate">
          {files.length} {files.length === 1 ? "file" : "files"} changed
          {within && <span title={within}> in {shortFolder(within)}</span>}
        </span>
        <Counts added={added} removed={removed} />
      </header>
      <ul>
        {listed.map((file) => (
          <li key={file.path}>
            <FileRow
              file={file}
              within={within}
              chosen={chosen === file.path}
              onChoose={() => open(file.path)}
            />
          </li>
        ))}
      </ul>
      {files.length > SHOWN && (
        <button
          type="button"
          onClick={() => open(files[SHOWN]!.path)}
          className="w-full px-3 py-1 text-left text-xs text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground"
        >
          {files.length - SHOWN} more {files.length - SHOWN === 1 ? "file" : "files"}
        </button>
      )}
    </section>
  );
}
