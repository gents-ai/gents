import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { PendingTray } from "../src/ui/screens/PendingTray";
import { moveItem } from "./pending-tray-menu";

const entry = (id: string): PendingQueueEntryView => ({
  requestDocId: `doc-${id}`,
  requestId: id,
  content: `message ${id}`,
  editable: true,
  editGroup: "same",
});
const turn = (row: PendingQueueEntryView): PendingTurnView => ({
  requestId: row.requestId,
  content: row.content,
  selectedSkillIds: [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin: null,
  createdAt: null,
});

describe("removing after a move", () => {
  it("still sends the removal once the move has settled", async () => {
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const queue = [entry("a"), entry("b"), entry("c")];
    const view = render(
      <PendingTray queued={queue.map(turn)} queue={queue} onEdit={onEdit} />,
    );
    fireEvent.click(await moveItem(screen.getAllByTestId("queued-input")[2]!, "up"));
    await waitFor(() => expect(onEdit).toHaveBeenCalledTimes(1));
    /* the refreshed queue: c and b swapped, under new ids as the runtime issues them */
    const moved = [entry("a"), entry("c2"), entry("b2")];
    view.rerender(
      <PendingTray queued={moved.map(turn)} queue={moved} onEdit={onEdit} />,
    );
    await waitFor(() =>
      expect(screen.getAllByRole("button", { name: "Remove" })[1]).toBeEnabled(),
    );
    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[1]!);
    await waitFor(() => expect(onEdit).toHaveBeenCalledTimes(2));
    expect(onEdit).toHaveBeenLastCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-c2", "doc-b2"],
      selectedRequestDocIds: ["doc-c2"],
      messages: [],
    });
  });
});
