import { useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Textarea } from "@gents/ui/components/textarea";
import type {
  PendingQueueEditRequest,
  PendingQueueEntryView,
} from "@source-inc/gents-desktop-client";

export type QueueEdit = Omit<PendingQueueEditRequest, "nodeDid" | "sessionId">;

export function PendingControls({
  entry,
  queue,
  onEdit,
}: {
  entry: PendingQueueEntryView;
  queue: PendingQueueEntryView[];
  onEdit: (edit: QueueEdit) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [content, setContent] = useState(entry.content);
  const [busy, setBusy] = useState(false);
  const index = queue.findIndex((item) => item.requestDocId === entry.requestDocId);
  const expectedRequestDocIds = queue.map((item) => item.requestDocId);
  const apply = async (
    selected: PendingQueueEntryView[],
    messages: QueueEdit["messages"],
  ) => {
    setBusy(true);
    try {
      await onEdit({
        expectedRequestDocIds,
        selectedRequestDocIds: selected.map((item) => item.requestDocId),
        messages,
      });
      setEditing(false);
    } catch {
      // The action owner reports the failure; keep the edit available.
    } finally {
      setBusy(false);
    }
  };
  const move = (offset: number) => {
    const neighbor = queue[index + offset];
    if (!neighbor) return;
    const selected = offset < 0 ? [neighbor, entry] : [entry, neighbor];
    void apply(
      selected,
      [...selected].reverse().map((item) => ({
        requestDocId: item.requestDocId,
        content: item.content,
      })),
    );
  };
  const canMove = (offset: number) => {
    const neighbor = queue[index + offset];
    return (
      neighbor?.editable &&
      entry.editGroup != null &&
      neighbor.editGroup === entry.editGroup
    );
  };
  return (
    <div
      className="flex flex-wrap items-center gap-1 pt-1"
      aria-label="Pending message controls"
    >
      {editing ? (
        <>
          <Textarea
            className="min-h-20 w-full text-sm"
            aria-label="Edit pending message"
            value={content}
            disabled={busy}
            onChange={(event) => setContent(event.target.value)}
          />
          <Button
            size="sm"
            variant="ghost"
            disabled={busy || !content.trim()}
            onClick={() =>
              void apply([entry], [{ requestDocId: entry.requestDocId, content }])
            }
          >
            Save
          </Button>
          <Button
            size="sm"
            variant="ghost"
            disabled={busy}
            onClick={() => setEditing(false)}
          >
            Cancel
          </Button>
        </>
      ) : (
        <Button
          size="sm"
          variant="ghost"
          disabled={busy}
          onClick={() => {
            setContent(entry.content);
            setEditing(true);
          }}
        >
          Edit
        </Button>
      )}
      <Button
        size="sm"
        variant="ghost"
        disabled={busy}
        onClick={() => void apply([entry], [])}
      >
        Remove
      </Button>
      <Button
        size="sm"
        variant="ghost"
        aria-label="Move pending message up"
        disabled={busy || !canMove(-1)}
        onClick={() => move(-1)}
      >
        Up
      </Button>
      <Button
        size="sm"
        variant="ghost"
        aria-label="Move pending message down"
        disabled={busy || !canMove(1)}
        onClick={() => move(1)}
      >
        Down
      </Button>
    </div>
  );
}
