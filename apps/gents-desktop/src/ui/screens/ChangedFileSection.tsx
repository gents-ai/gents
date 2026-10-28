import { lazy, memo, Suspense, useState } from "react";
import {
  ChevronRight,
  Copy,
  EllipsisVertical,
  FoldVertical,
  FolderOpen,
  UnfoldVertical,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Skeleton } from "@gents/ui/components/skeleton";
import { cn } from "@gents/ui/lib/utils";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { shortPath } from "@/lib/paths";
import { useRecordedDiffReader } from "../../hooks/fileChanges";
import type { SessionScope } from "../../lib/recordedFiles";
import { patchText } from "@/lib/filePatch";
import { relativeTo, type ChangedFile } from "@/lib/turnChanges";
import { revealInFolder } from "../../lib/nativeShell";
import { revealInFolderLabel } from "../../lib/shellPlatform";
import { isAbsolutePath } from "./tool-summary";
import { Counts, FileLabel, fileName } from "./TurnChanges";

const RecordedDiffView = lazy(() => import("./RecordedDiffView"));

/** One changed file in the Changes panel: a header that collapses it and
    holds its menu, then its diff. The diff is read and drawn only once the
    file comes near the visible part of the panel. Memoised so that following
    the current file while scrolling does not draw the diffs again. */
export const ChangedFileSection = memo(function ChangedFileSection({
  scope,
  file,
  within,
  open,
  near,
  remote,
  onToggle,
  sections,
}: {
  scope: SessionScope;
  file: ChangedFile;
  within: string;
  open: boolean;
  near: boolean;
  /** the session ran on another node, whose copies come over the network */
  remote: boolean;
  onToggle: (path: string) => void;
  /** where the panel finds each file's section to scroll to and watch */
  sections: Map<string, HTMLElement>;
}) {
  const [whole, setWhole] = useState(false);
  const [draws, setDraws] = useState(0);
  return (
    <section
      ref={(el) => {
        if (!el) return;
        sections.set(file.path, el);
        return () => {
          sections.delete(file.path);
        };
      }}
      aria-label={file.path}
      data-path={file.path}
    >
      <div className="sticky top-0 z-10 flex items-center bg-surface pr-1">
        <button
          type="button"
          aria-expanded={open}
          onClick={() => onToggle(file.path)}
          className="flex min-w-0 flex-1 items-center justify-between gap-3 py-1.5 pr-2 pl-3 text-left text-sm outline-none hover:bg-foreground/5 focus-visible:bg-foreground/5"
        >
          <span className="flex min-w-0 items-center gap-1.5">
            <ChevronRight
              className={cn(
                "size-3.5 shrink-0 text-muted-foreground transition-transform",
                open && "rotate-90",
              )}
            />
            <FileLabel path={file.path} within={within} />
          </span>
          <Counts added={file.added} removed={file.removed} />
        </button>
        <FileMenu
          scope={scope}
          file={file}
          within={within}
          remote={remote}
          whole={open ? whole : null}
          onWhole={(next) => {
            /* hiding draws the diff anew, so lines opened from its own
               separators fold back too */
            if (!next) setDraws((count) => count + 1);
            setWhole(next);
          }}
        />
      </div>
      {open &&
        (near ? (
          <Suspense fallback={<Loading remote={remote} file={file} />}>
            <RecordedDiffView key={draws} scope={scope} file={file} whole={whole} />
          </Suspense>
        ) : (
          <Loading remote={remote} file={file} />
        ))}
    </section>
  );
});

/** What can be done with one changed file: show all its lines or only those
    around its changes, copy its path or its recorded change, and, for a
    session on this computer, show it in its folder. */
function FileMenu({
  scope,
  file,
  within,
  remote,
  whole,
  onWhole,
}: {
  scope: SessionScope;
  file: ChangedFile;
  within: string;
  remote: boolean;
  /** whether the open diff shows every line; null while it is closed */
  whole: boolean | null;
  onWhole: (whole: boolean) => void;
}) {
  const readDiff = useRecordedDiffReader(scope);
  const home = useHomeFolder();
  const reveal = !remote && isAbsolutePath(file.path) ? revealInFolderLabel() : null;
  const copy = (text: string) => void navigator.clipboard?.writeText(text);
  const copyPatch = async () => {
    const diff = await readDiff(file);
    if (diff.kind === "text") copy(patchText(file.path, diff.before, diff.after));
    else toast("The recorded versions of this file could not be read.");
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="icon-sm"
            aria-label={`${fileName(file.path)} actions`}
            className="shrink-0"
          />
        }
      >
        <EllipsisVertical />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-auto min-w-48">
        {whole !== null && (
          <>
            <DropdownMenuItem onClick={() => onWhole(!whole)}>
              {whole ? <FoldVertical /> : <UnfoldVertical />}
              {whole ? "Hide unchanged lines" : "Show all lines"}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        )}
        <DropdownMenuItem onClick={() => copy(file.path)}>
          <Copy />
          Copy path
        </DropdownMenuItem>
        {within && (
          <DropdownMenuItem onClick={() => copy(relativeTo(within, file.path))}>
            <Copy />
            Copy relative path
          </DropdownMenuItem>
        )}
        <DropdownMenuItem onClick={() => void copyPatch()}>
          <Copy />
          Copy diff
        </DropdownMenuItem>
        {reveal && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onClick={() =>
                void revealInFolder(file.path).catch((error: unknown) =>
                  toast(
                    `Couldn't find ${shortPath(file.path, { home })} on this computer: ${String(error)}`,
                  ),
                )
              }
            >
              <FolderOpen />
              {reveal}
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const SKELETON_WIDTHS = ["55%", "70%", "40%", "85%", "62%", "30%"];

/* about as many rows as the edit changed, with a line around them */
const skeletonRows = (file: ChangedFile) =>
  Math.min(SKELETON_WIDTHS.length, Math.max(3, file.added + file.removed + 2));

/* about the height the diff will take, so little moves when it arrives: its
   changed lines, a few unchanged ones around each edit, and the bars that
   stand for the rest */
const diffHeight = (file: ChangedFile) =>
  Math.min(
    480,
    18 * (file.added + file.removed + 6 * file.edits.length) +
      36 * (file.edits.length + 1),
  );

/* The shape of a diff while it loads: rows at the diff's height, as many
   as the edit suggests, a number and a line of code each. A quick load
   ends before it fades in. Another node's copies come over the network
   and can take a while, so that wait is also said. */
function Loading({ remote, file }: { remote: boolean; file: ChangedFile }) {
  return (
    <div
      role="status"
      aria-label="Loading changes"
      className="animate-[appear-late_150ms_ease-out_200ms_both] px-3 py-2"
      style={{ minHeight: diffHeight(file) }}
    >
      {remote && (
        <p className="pb-2 text-sm text-muted-foreground">
          Loading changes from the node that made them…
        </p>
      )}
      {SKELETON_WIDTHS.slice(0, skeletonRows(file)).map((width, i) => (
        <div key={i} className="flex h-[18px] items-center gap-3">
          <Skeleton className="h-2.5 w-5 shrink-0 rounded-sm bg-foreground/10" />
          <Skeleton className="h-2.5 rounded-sm bg-foreground/10" style={{ width }} />
        </div>
      ))}
    </div>
  );
}
