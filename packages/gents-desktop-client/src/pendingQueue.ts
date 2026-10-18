import type {
  PendingQueueEditRequest,
  PendingQueueEntryView,
  PendingTurnView,
} from "./types.js";

export type QueueEdit = Omit<PendingQueueEditRequest, "nodeDid" | "sessionId">;

export type PendingRow =
  | { kind: "automated"; turn: PendingTurnView }
  | {
      kind: "message";
      turn: PendingTurnView;
      entry: PendingQueueEntryView | null;
      index: number;
    };

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
