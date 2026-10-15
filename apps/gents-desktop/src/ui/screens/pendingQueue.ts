/* What the pending tray shows and what an edit to it sends, kept apart from
   the tray's rendering. An edit always carries the whole observed queue as
   its fence; the runtime refuses one made against a queue that has since
   changed. */
import type {
  PendingQueueEditRequest,
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";

export type QueueEdit = Omit<PendingQueueEditRequest, "nodeDid" | "sessionId">;

export type PendingRow =
  | { kind: "automated"; turn: PendingTurnView }
  | {
      kind: "message";
      turn: PendingTurnView;
      entry: PendingQueueEntryView | null;
      index: number;
    };

/* The native snapshot owns arrival order, including already claimed inputs.
   Queue controls determine editability without reordering that snapshot. */
export function pendingRows(
  queued: readonly PendingTurnView[],
  queue: readonly PendingQueueEntryView[] | null,
): PendingRow[] {
  const listed = new Map(
    queue?.map((entry, index) => [entry.requestId, { entry, index }]),
  );
  return queued.map((turn) => {
    if (turn.origin) return { kind: "automated", turn };
    const slot = listed.get(turn.requestId);
    return {
      kind: "message",
      turn,
      entry: slot?.entry.editable ? slot.entry : null,
      index: slot?.entry.editable ? slot.index : -1,
    };
  });
}

/* A move rewrites the contiguous run it crosses. Every entry in that run
   must be editable and in the moved entry's execution group, so an input
   never jumps over another group or a message it may not edit. */
export function moveRun(
  queue: readonly PendingQueueEntryView[],
  from: number,
  to: number,
): PendingQueueEntryView[] | null {
  if (from === to || !queue[from] || !queue[to]) return null;
  const group = queue[from].editGroup;
  if (group == null) return null;
  const run = queue.slice(Math.min(from, to), Math.max(from, to) + 1);
  return run.every((item) => item.editable && item.editGroup === group) ? run : null;
}

/* Two of a person's messages the runtime groups apart: what sets a group
   is the runtime's, so the tray says only that they differ. */
export function separatedBySettings(
  queue: readonly PendingQueueEntryView[],
  a: number,
  b: number,
): boolean {
  const first = queue[a];
  const second = queue[b];
  return Boolean(
    first?.editable &&
    second?.editable &&
    first.editGroup != null &&
    second.editGroup != null &&
    first.editGroup !== second.editGroup,
  );
}

const fence = (queue: readonly PendingQueueEntryView[]) =>
  queue.map((item) => item.requestDocId);

export function moveEdit(
  queue: readonly PendingQueueEntryView[],
  from: number,
  to: number,
): QueueEdit | null {
  const run = moveRun(queue, from, to);
  if (!run) return null;
  const start = Math.min(from, to);
  const reordered = [...run];
  const [moved] = reordered.splice(from - start, 1);
  reordered.splice(to - start, 0, moved!);
  return {
    expectedRequestDocIds: fence(queue),
    selectedRequestDocIds: run.map((item) => item.requestDocId),
    messages: reordered.map((item) => ({
      requestDocId: item.requestDocId,
      content: item.content,
    })),
  };
}

export function rewriteEdit(
  queue: readonly PendingQueueEntryView[],
  entry: PendingQueueEntryView,
  content: string,
): QueueEdit {
  return {
    expectedRequestDocIds: fence(queue),
    selectedRequestDocIds: [entry.requestDocId],
    messages: [{ requestDocId: entry.requestDocId, content }],
  };
}

export function removeEdit(
  queue: readonly PendingQueueEntryView[],
  entry: PendingQueueEntryView,
): QueueEdit {
  return {
    expectedRequestDocIds: fence(queue),
    selectedRequestDocIds: [entry.requestDocId],
    messages: [],
  };
}
