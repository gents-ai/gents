import { use, useCallback, useEffect } from "react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { fileId, forgetUnreachable, queued } from "../lib/fileReads";
import {
  recordedDiff,
  type RecordedDiff,
  type SessionScope,
} from "../lib/recordedFiles";
import { useApp } from "../ui/app/AppContext";
import type { ChangedFile } from "../ui/lib/turnChanges";

type Fetch = DesktopApiAdapter["fetchToolFileChanges"];

/* Recorded versions never change, so a read is kept while its diff is on
   screen, and the most recent few others after, since each holds up to two
   file copies. A read still under way is never dropped: a diff waiting on
   it has not yet held it, and dropping it would only start it again. A
   failed read is kept until the reader tries again, which retries every
   failed file at once. */
const KEPT = 16;
const reads = new Map<string, Promise<RecordedDiff>>();
const settled = new Set<string>();
const shownBy = new Map<string, number>();
const failed = new Set<string>();
const attempts = createStore(() => 0);

function trim() {
  for (const key of reads.keys()) {
    if (reads.size <= KEPT) return;
    if (settled.has(key) && !shownBy.has(key)) {
      reads.delete(key);
      settled.delete(key);
    }
  }
}

/** The key a changed file's recorded versions are kept under. */
export const recordedKey = (scope: SessionScope, file: ChangedFile) =>
  [
    scope.nodeDid,
    scope.sessionId,
    file.path,
    ...file.edits.map((e) => e.toolCallDocId),
  ].join("\0");

function read(fetch: Fetch | undefined, scope: SessionScope, file: ChangedFile) {
  const key = recordedKey(scope, file);
  const held = reads.get(key);
  if (held) return held;
  const fresh = (
    fetch
      ? recordedDiff(queued(fetch, fileId(scope, file.path)), scope, file)
      : Promise.resolve<RecordedDiff>({
          kind: "unavailable",
          reason: "Recorded versions are not available here.",
        })
  ).then((diff) => {
    settled.add(key);
    if (diff.kind === "unavailable") failed.add(key);
    return diff;
  });
  reads.set(key, fresh);
  return fresh;
}

/** Reads every failed file again. */
export function retryRecordedDiffs() {
  forgetUnreachable();
  for (const key of failed) {
    reads.delete(key);
    settled.delete(key);
  }
  failed.clear();
  attempts.setState((n) => n + 1, true);
}

/** What a turn did to `file`, for a view to show: suspends until it is
    read, and keeps the read while the view is on screen. */
export function useRecordedDiff(scope: SessionScope, file: ChangedFile): RecordedDiff {
  const { api } = useApp();
  const key = recordedKey(scope, file);
  useStore(attempts);
  useEffect(() => {
    shownBy.set(key, (shownBy.get(key) ?? 0) + 1);
    return () => {
      const left = shownBy.get(key)! - 1;
      if (left) shownBy.set(key, left);
      else shownBy.delete(key);
      trim();
    };
  }, [key]);
  return use(read(api.fetchToolFileChanges, scope, file));
}

/** Reads a changed file's recorded versions outside a view: ahead of the
    reader, or to copy them. */
export function useRecordedDiffReader(scope: SessionScope) {
  const { api } = useApp();
  return useCallback(
    (file: ChangedFile) => read(api.fetchToolFileChanges, scope, file),
    [api, scope],
  );
}
