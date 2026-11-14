/* The open session's rows beyond its newest page, read by message number
   as the reader nears either end of what is drawn. One read at a time; a
   read that fails is made again shortly. */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import type {
  RenderedTimelineItem,
  SessionRowsView,
  SessionRowsWindow,
} from "@source-inc/gents-desktop-client";
import { acceptsAsyncResult } from "../../hooks/observationOrdering";
import { applyRowsRead, drawnRows, noRows } from "./transcriptRows";

const RETRY_MS = [500, 1000, 2000, 4000];

const ownerKey = (nodeDid: string | null, sessionId: string | null) =>
  sessionId === null ? null : `${nodeDid ?? ""}\0${sessionId}`;

export function useHeldRows(
  nodeDid: string | null,
  sessionId: string | null,
  newest: readonly RenderedTimelineItem[],
  newestHasOlder: boolean,
  load: (window: SessionRowsWindow) => Promise<SessionRowsView | null>,
) {
  const key = ownerKey(nodeDid, sessionId);
  const [held, setHeld] = useState(() => ({ key, rows: noRows(sessionId) }));
  const current = held.key === key ? held : { key, rows: noRows(sessionId) };
  if (held !== current) setHeld(current);
  const drawn = useMemo(
    () => drawnRows(current.rows, [...newest], newestHasOlder),
    [current.rows, newest, newestHasOlder],
  );
  const [loading, setLoading] = useState<"older" | "newer" | "jump" | null>(null);
  const [failure, setFailure] = useState<{ message: string; denied: boolean } | null>(
    null,
  );
  const failedRead = useRef<(() => void) | null>(null);
  const busy = useRef(false);
  const failures = useRef(0);
  const retry = useRef<ReturnType<typeof setTimeout> | null>(null);

  const latest = useRef(0);
  const owner = useRef(key);
  owner.current = key;
  useEffect(
    () => () => {
      if (retry.current) clearTimeout(retry.current);
      latest.current += 1;
      busy.current = false;
      failures.current = 0;
      failedRead.current = null;
      setLoading(null);
      setFailure(null);
    },
    [key],
  );

  /* `apply` lets the caller choose when the rows land, so rows added above
     the reader wait for the view to rest */
  const read = useCallback(
    (
      window: SessionRowsWindow,
      kind: "older" | "newer" | "jump",
      apply: (land: () => void) => void = (land) => land(),
      landed?: () => void,
    ) => {
      if (busy.current) return;
      if (retry.current) clearTimeout(retry.current);
      busy.current = true;
      const mine = ++latest.current;
      setLoading(kind);
      setFailure(null);
      failedRead.current = null;
      const reader = owner.current;
      const settles = () => {
        if (!acceptsAsyncResult(latest.current, mine)) return false;
        busy.current = false;
        setLoading(null);
        return true;
      };
      load(window).then(
        (view) => {
          if (!settles() || !view) return;
          if (view.unreadable) {
            setFailure({ message: view.unreadable, denied: false });
            failedRead.current = () => read(window, kind, apply, landed);
            return;
          }
          failures.current = 0;

          apply(() => {
            setHeld((now) =>
              now.key === reader
                ? { key: reader, rows: applyRowsRead(now.rows, window, view) }
                : now,
            );
            landed?.();
          });
        },
        (error: unknown) => {
          if (!settles()) return;
          const denied =
            error instanceof BridgeInvokeError && error.code === "permissionDenied";
          setFailure({
            message: denied
              ? "This conversation is not shared with this device."
              : "Couldn’t load these messages.",
            denied,
          });
          if (denied) {
            setHeld((now) =>
              now.key === reader
                ? { key: reader, rows: noRows(now.rows.sessionId) }
                : now,
            );
            return;
          }
          failedRead.current = () => read(window, kind, apply, landed);
          if (error instanceof BridgeInvokeError && error.retryable) {
            const delay = RETRY_MS[Math.min(failures.current, RETRY_MS.length - 1)]!;
            failures.current += 1;
            retry.current = setTimeout(() => read(window, kind, apply, landed), delay);
          }
        },
      );
    },
    [load],
  );

  const { olderBefore, newerAfter } = drawn;
  const loadOlder = useCallback(
    (apply?: (land: () => void) => void) => {
      if (olderBefore !== null)
        read({ kind: "before", sequence: olderBefore }, "older", apply);
    },
    [olderBefore, read],
  );
  const loadNewer = useCallback(() => {
    if (newerAfter !== null) read({ kind: "after", sequence: newerAfter }, "newer");
  }, [newerAfter, read]);
  /* the page around a message, drawn apart from the newest page until the
     reads between close the gap */
  const jump = useCallback(
    (sequence: number, landed?: () => void) => {
      busy.current = false;
      read({ kind: "around", sequence }, "jump", undefined, landed);
    },
    [read],
  );
  const reset = useCallback(
    () => setHeld((now) => ({ ...now, rows: noRows(now.rows.sessionId) })),
    [],
  );

  const retryRead = useCallback(() => {
    if (retry.current) clearTimeout(retry.current);
    failedRead.current?.();
  }, []);

  return { ...drawn, loading, failure, retryRead, loadOlder, loadNewer, jump, reset };
}
