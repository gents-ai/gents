import type { ReactNode } from "react";
import { cn } from "@gents/ui/lib/utils";
import { useDock } from "@/app/workspace";
import { CARD_FILES } from "@/lib/rowHeights";
import type { SessionScope } from "../../lib/recordedFiles";
import {
  netCounts,
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

/** A file's net line counts; a file edited more than once shows how many
    times instead, since its edits' diffs do not add up to its change, and
    a file whose turn began before the loaded rows shows neither. */
export function FileCounts({ file }: { file: ChangedFile }) {
  const net = netCounts(file);
  if (net) return <Counts {...net} />;
  if (!file.whole) return null;
  return (
    <span className="shrink-0 text-xs text-muted-foreground">
      {file.edits.length} edits
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
  mark,
}: {
  file: ChangedFile;
  within: string;
  chosen: boolean;
  onChoose: () => void;
  /** shown after the file's name, such as that it was viewed */
  mark?: ReactNode;
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
      <span className="flex min-w-0 items-center gap-1.5">
        <FileLabel path={file.path} within={within} />
        {mark}
      </span>
      <FileCounts file={file} />
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
  const chosen = useChosenPath(scope, turn);
  const listed = files.slice(0, SHOWN);
  const nets = files.map(netCounts);
  const total = nets.every((net) => net !== null)
    ? {
        added: nets.reduce((n, net) => n + net!.added, 0),
        removed: nets.reduce((n, net) => n + net!.removed, 0),
      }
    : null;
  const partial = files.some((f) => !f.whole);
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
        {partial ? (
          <span
            className="shrink-0"
            title="This turn began before the loaded messages; scroll up to load the rest"
          >
            Partial
          </span>
        ) : (
          total && <Counts {...total} />
        )}
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
