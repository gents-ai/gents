import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { QueuedInputs } from "../src/ui/screens/TranscriptInputs";

const entry = (id: string, group: string | null = "same"): PendingQueueEntryView => ({
  requestDocId: `doc-${id}`,
  requestId: id,
  content: `message ${id}`,
  editable: group !== null,
  editGroup: group,
});
const turn = (row: PendingQueueEntryView): PendingTurnView => ({
  requestId: row.requestId,
  requestDocId: row.requestDocId,
  content: row.content,
  selectedSkillIds: [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin: null,
  createdAt: null,
});

function setup(queue: PendingQueueEntryView[]) {
  const onEdit = vi.fn().mockResolvedValue(undefined);
  render(
    <QueuedInputs
      queued={queue.filter((row) => row.editable).map(turn)}
      queue={queue}
      onEdit={onEdit}
    />,
  );
  return onEdit;
}

describe("pending message controls", () => {
  it("edits text with the complete observed queue fence and physical source identity", async () => {
    const onEdit = setup([entry("a"), entry("barrier", null), entry("b")]);
    fireEvent.click(screen.getAllByRole("button", { name: "Edit" })[0]);
    fireEvent.change(screen.getByRole("textbox", { name: "Edit pending message" }), {
      target: { value: "corrected instruction" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith({
        expectedRequestDocIds: ["doc-a", "doc-barrier", "doc-b"],
        selectedRequestDocIds: ["doc-a"],
        messages: [{ requestDocId: "doc-a", content: "corrected instruction" }],
      }),
    );
    expect(
      screen.getAllByRole("button", { name: "Move pending message down" })[0],
    ).toBeDisabled();
  });

  it("removes only the selected pending source and preserves the whole queue fence", async () => {
    const onEdit = setup([entry("a"), entry("b")]);
    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[1]);
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith({
        expectedRequestDocIds: ["doc-a", "doc-b"],
        selectedRequestDocIds: ["doc-b"],
        messages: [],
      }),
    );
  });

  it("reorders adjacent compatible sources without changing their text or observed order", async () => {
    const onEdit = setup([entry("a"), entry("b")]);
    fireEvent.click(
      screen.getAllByRole("button", { name: "Move pending message down" })[0],
    );
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith({
        expectedRequestDocIds: ["doc-a", "doc-b"],
        selectedRequestDocIds: ["doc-a", "doc-b"],
        messages: [
          { requestDocId: "doc-b", content: "message b" },
          { requestDocId: "doc-a", content: "message a" },
        ],
      }),
    );
    expect(screen.getAllByTestId("queued-input").map((row) => row.textContent)).toEqual(
      [expect.stringContaining("message a"), expect.stringContaining("message b")],
    );
  });

  it("cannot move a source across a different execution group", () => {
    setup([entry("a"), entry("b", "other")]);
    for (const button of screen.getAllByRole("button", {
      name: /Move pending message/,
    })) {
      expect(button).toBeDisabled();
    }
  });
});
