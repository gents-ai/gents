import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { cn } from "@gents/ui/lib/utils";
import type { SurfaceContext } from "@/app/surfaces";
import { viewedFiles } from "@/app/viewedFiles";
import { useRecordedDiffReader } from "../../hooks/fileChanges";
import { useHomeDid } from "@/hooks/useClient";
import { useSelectedSessionValue } from "@/hooks/useSelectedSession";
import { thinScrollbar } from "@/lib/scrollbar";
import { sharedFolder } from "@/lib/turnChanges";
import { ChangedFileSection } from "./ChangedFileSection";
import { ChangesFileList } from "./ChangesFileList";
import { ChangesPicker } from "./ChangesPicker";
import {
  changesView,
  useFileListOpen,
  useShownChanges,
  type ShownChanges,
} from "./changes";

export function ChangesSurface({ sessionId }: SurfaceContext) {
  const nodeDid = useSelectedSessionValue((session) =>
    session && session.sessionId === sessionId ? session.nodeDid : null,
  );
  const scope = sessionId && nodeDid ? { nodeDid, sessionId } : null;
  const shown = useShownChanges(scope);
  if (!scope || !shown)
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Choose a file under a reply to see what the turn changed.
      </p>
    );
  return (
    <TurnDiffs
      key={JSON.stringify([scope.nodeDid, scope.sessionId, shown.turn])}
      shown={shown}
    />
  );
}

/* how far beyond the visible part of the panel a file's diff is drawn, and
   further, its recorded versions read, ahead of the reader */
const NEAR_MARGIN = "600px 0px";
const READ_AHEAD = "2000px 0px";

const withPath = (set: ReadonlySet<string>, path: string) =>
  set.has(path) ? set : new Set([...set, path]);

/** Every file the turn changed, in one scroll; the file chosen under the
    reply, or in the picker, is opened and brought to the top. */
function TurnDiffs({ shown }: { shown: ShownChanges }) {
  const { files } = shown;
  const within = useMemo(() => sharedFolder(files.map((f) => f.path)), [files]);
  const row = useRef<HTMLDivElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const sections = useRef(new Map<string, HTMLElement>());
  const pinned = useRef<string | null>(null);
  const align = useRef(() => {});
  const [current, setCurrent] = useState(shown.path);
  /* files marked viewed start folded away */
  const [closed, setClosed] = useState<ReadonlySet<string>>(
    () =>
      new Set(
        files
          .filter((f) => f.path !== shown.path && viewedFiles.isViewed(shown.scope, f))
          .map((f) => f.path),
      ),
  );
  const [near, setNear] = useState<ReadonlySet<string>>(() => new Set([shown.path]));
  const listOpen = useFileListOpen();
  const home = useHomeDid();
  const remote = home !== null && shown.scope.nodeDid !== home;
  const readAhead = useRecordedDiffReader(shown.scope);

  /* Diffs above the chosen file load after it is scrolled to and push it
     down, so it is held at the top until the reader scrolls. Sections are
     measured when sizes change rather than on scroll: in WebKit, measuring
     on every scroll event made scrolling stutter. */
  useLayoutEffect(() => {
    const viewport = scroller.current;
    const list = content.current;
    if (!viewport || !list) return;
    let tops: number[] = [];
    let anchor: { at: number; offset: number } | null = null;
    const measure = () => {
      tops = files.map((f) => sections.current.get(f.path)?.offsetTop ?? 0);
    };
    /* WebKit keeps no scroll anchor, so a file above the reader that grows
       as its diff arrives would push what they are reading down; the file
       at the top holds its place instead. */
    align.current = () => {
      measure();
      const at = files.findIndex((f) => f.path === pinned.current);
      if (at >= 0) viewport.scrollTop = tops[at]!;
      else if (anchor) viewport.scrollTop = tops[anchor.at]! + anchor.offset;
    };
    /* the file at the top is the current one; at the end of the scroll the
       last file may never reach the top, so the end counts as the last */
    let frame = 0;
    const track = () => {
      if (pinned.current || frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const top = viewport.scrollTop + 1;
        let at = 0;
        tops.forEach((t, i) => {
          if (t <= top) at = i;
        });
        anchor = { at, offset: viewport.scrollTop - tops[at]! };
        if (top + viewport.clientHeight >= viewport.scrollHeight) at = files.length - 1;
        setCurrent(files[at]!.path);
      });
    };
    const release = () => {
      pinned.current = null;
    };
    const sizes = new ResizeObserver(() => align.current());
    sizes.observe(list);
    const nearby = new IntersectionObserver(
      (entries) => {
        const coming = entries
          .filter((entry) => entry.isIntersecting)
          .map((entry) => (entry.target as HTMLElement).dataset.path!);
        if (coming.length) setNear((was) => coming.reduce(withPath, was));
      },
      { root: viewport, rootMargin: NEAR_MARGIN },
    );
    const byPath = new Map(files.map((f) => [f.path, f]));
    const ahead = new IntersectionObserver(
      (entries) => {
        const coming = entries
          .filter((entry) => entry.isIntersecting)
          .map((entry) => byPath.get((entry.target as HTMLElement).dataset.path!)!);
        if (!coming.length) return;
        for (const file of coming) void readAhead(file);
      },
      { root: viewport, rootMargin: READ_AHEAD },
    );
    for (const section of sections.current.values()) {
      nearby.observe(section);
      ahead.observe(section);
    }
    const reader = ["wheel", "touchstart", "pointerdown", "keydown"] as const;
    viewport.addEventListener("scroll", track, { passive: true });
    for (const e of reader) viewport.addEventListener(e, release, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      sizes.disconnect();
      nearby.disconnect();
      ahead.disconnect();
      viewport.removeEventListener("scroll", track);
      for (const e of reader) viewport.removeEventListener(e, release);
    };
  }, [files]);

  useLayoutEffect(() => {
    pinned.current = shown.path;
    setCurrent(shown.path);
    setNear((was) => withPath(was, shown.path));
    setClosed((was) => {
      if (!was.has(shown.path)) return was;
      const next = new Set(was);
      next.delete(shown.path);
      return next;
    });
    align.current();
  }, [shown]);

  const choose = (path: string) => changesView.choose(shown.scope, path);
  const toggle = useCallback(
    (path: string) =>
      setClosed((was) => {
        const next = new Set(was);
        if (!next.delete(path)) next.add(path);
        return next;
      }),
    [],
  );

  return (
    <div className="@container flex h-full min-h-0 flex-col">
      {files.length > 1 && (
        <ChangesPicker
          files={files}
          within={within}
          index={Math.max(
            0,
            files.findIndex((f) => f.path === current),
          )}
          onChoose={choose}
        />
      )}
      <div ref={row} className="flex min-h-0 flex-1">
        <div
          ref={scroller}
          className={cn(
            "min-h-0 min-w-0 flex-1 overflow-y-auto [overflow-anchor:none]",
            thinScrollbar,
          )}
        >
          <div ref={content} className="relative">
            {files.map((file) => (
              <ChangedFileSection
                key={file.path}
                scope={shown.scope}
                file={file}
                within={within}
                open={!closed.has(file.path)}
                near={near.has(file.path)}
                remote={remote}
                onToggle={toggle}
                sections={sections.current}
              />
            ))}
            <p className="px-3 py-3 text-xs text-muted-foreground">
              Shows edits made by file tools. Changes made by commands are not recorded.
            </p>
          </div>
        </div>
        {files.length > 1 && listOpen && (
          <ChangesFileList
            scope={shown.scope}
            files={files}
            within={within}
            current={current}
            row={row}
            onChoose={choose}
          />
        )}
      </div>
    </div>
  );
}
