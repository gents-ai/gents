import {
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from "react";
import { cn } from "@gents/ui/lib/utils";
import { thinScrollbar } from "@/lib/scrollbar";
import { shortFolder, type ChangedFile } from "@/lib/turnChanges";
import { useViewed } from "@/app/viewedFiles";
import type { SessionScope } from "../../lib/recordedFiles";
import { keepFileListWidth, setFileListWidth, useFileListWidth } from "./changes";
import { FileContextMenu, ViewedMark } from "./ChangedFileSection";
import { FileRow } from "./TurnChanges";

const LIST_MIN = 140;

/** A turn's files beside their diffs, in a panel wide enough for both;
    its handle drags it wider. */
export function ChangesFileList({
  scope,
  remote,
  onViewed,
  files,
  within,
  current,
  row,
  onChoose,
}: {
  scope: SessionScope;
  /** the session ran on another node */
  remote: boolean;
  onViewed: (file: ChangedFile, viewed: boolean) => void;
  files: readonly ChangedFile[];
  within: string;
  current: string;
  /** the panel row the list shares with the diffs, which bounds its width */
  row: RefObject<HTMLDivElement | null>;
  onChoose: (path: string) => void;
}) {
  const width = useFileListWidth();
  const box = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={box}
      className={cn(
        "relative hidden max-w-[60%] shrink-0 @lg:flex",
        width === null && "w-44 @3xl:w-60",
      )}
      style={width === null ? undefined : { width }}
    >
      <ListHandle list={box} row={row} />
      <nav
        aria-label="Files changed in this turn"
        className={cn(
          "flex min-w-0 flex-1 flex-col overflow-y-auto border-l border-border py-1 text-sm",
          thinScrollbar,
        )}
      >
        {within && (
          <p
            className="truncate px-3 py-1 text-xs text-muted-foreground"
            title={within}
          >
            {shortFolder(within)}
          </p>
        )}
        {files.map((f) => (
          <ListedFile
            key={f.path}
            scope={scope}
            remote={remote}
            onViewed={onViewed}
            file={f}
            within={within}
            chosen={f.path === current}
            onChoose={() => onChoose(f.path)}
          />
        ))}
      </nav>
    </div>
  );
}

/* a file in the list, whose right-click holds the file's own items */
function ListedFile({
  scope,
  remote,
  onViewed,
  ...row
}: Omit<Parameters<typeof FileRow>[0], "mark"> & {
  scope: SessionScope;
  remote: boolean;
  onViewed: (file: ChangedFile, viewed: boolean) => void;
}) {
  const viewed = useViewed(scope, row.file);
  return (
    <FileContextMenu
      scope={scope}
      file={row.file}
      within={row.within}
      remote={remote}
      viewed={viewed}
      onViewed={(next) => onViewed(row.file, next)}
      whole={null}
      render={<div />}
    >
      <FileRow {...row} mark={viewed ? <ViewedMark /> : undefined} />
    </FileContextMenu>
  );
}

/** Drags the file list wider or narrower, up to most of the panel; arrow
    keys step it, and a double click returns it to its own width. */
function ListHandle({
  list,
  row,
}: {
  list: RefObject<HTMLDivElement | null>;
  row: RefObject<HTMLDivElement | null>;
}) {
  const width = useFileListWidth();
  const clamp = (w: number) =>
    Math.min(
      Math.max(LIST_MIN, (row.current?.clientWidth ?? 0) * 0.6),
      Math.max(LIST_MIN, w),
    );
  const now = () => list.current?.getBoundingClientRect().width ?? LIST_MIN;
  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const handle = event.currentTarget;
    handle.setPointerCapture(event.pointerId);
    const startX = event.clientX;
    const start = now();
    document.documentElement.dataset.dragging = "divider";
    const move = (e: PointerEvent) =>
      setFileListWidth(clamp(start + startX - e.clientX));
    const end = () => {
      delete document.documentElement.dataset.dragging;
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
      keepFileListWidth();
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };
  const onKeyDown = (event: ReactKeyboardEvent) => {
    const step = event.key === "ArrowLeft" ? 16 : event.key === "ArrowRight" ? -16 : 0;
    if (!step) return;
    event.preventDefault();
    setFileListWidth(clamp(now() + step));
    keepFileListWidth();
  };
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize file list"
      aria-valuenow={width === null ? undefined : Math.round(width)}
      aria-valuemin={LIST_MIN}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
      onDoubleClick={() => {
        setFileListWidth(null);
        keepFileListWidth();
      }}
      className="group absolute top-0 -left-1 z-20 flex h-full w-2 cursor-col-resize items-center justify-center outline-none focus-visible:bg-accent"
    >
      <div className="h-10 w-0.5 rounded-full bg-border transition-colors group-hover:bg-muted-foreground group-focus-visible:bg-ring" />
    </div>
  );
}
