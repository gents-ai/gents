import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

function deferred() {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function rowFor(id: string) {
  const row = screen
    .getAllByTestId("queued-input")
    .find((node) => node.textContent?.includes(`message ${id}`));
  if (!row) throw new Error(`no queued row for ${id}`);
  return row;
}

function rows() {
  return screen.getAllByTestId("queued-input").map((row) => row.textContent);
}

describe("pending message failed mutations and async controls", () => {
  it("keeps the edited text visible and the controls usable after a failed save", async () => {
    const onEdit = vi.fn().mockRejectedValueOnce(new Error("mutation failed"));
    render(
      <QueuedInputs queued={[turn(entry("a"))]} queue={[entry("a")]} onEdit={onEdit} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Edit pending message" }), {
      target: { value: "corrected instruction" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    /* the failure is reported by the action owner; the edit stays on screen */
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Save" })).toBeEnabled(),
    );
    expect(screen.getByRole("textbox", { name: "Edit pending message" })).toHaveValue(
      "corrected instruction",
    );
    expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Remove" })).toBeEnabled();
    expect(rows()).toEqual([expect.stringContaining("message a")]);

    /* the preserved text is what the retry sends */
    onEdit.mockResolvedValueOnce(undefined);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(onEdit).toHaveBeenCalledTimes(2));
    expect(onEdit).toHaveBeenLastCalledWith({
      expectedRequestDocIds: ["doc-a"],
      selectedRequestDocIds: ["doc-a"],
      messages: [{ requestDocId: "doc-a", content: "corrected instruction" }],
    });
    await waitFor(() =>
      expect(
        screen.queryByRole("textbox", { name: "Edit pending message" }),
      ).toBeNull(),
    );
  });

  it("disables the edit text and this row's controls while its mutation is in flight", async () => {
    const gate = deferred();
    const onEdit = vi.fn().mockReturnValue(gate.promise);
    render(
      <QueuedInputs
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("b")]}
        onEdit={onEdit}
      />,
    );
    const controls = within(rowFor("a"));
    fireEvent.click(controls.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Edit pending message" }), {
      target: { value: "corrected instruction" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onEdit).toHaveBeenCalled());
    expect(
      screen.getByRole("textbox", { name: "Edit pending message" }),
    ).toBeDisabled();
    for (const name of ["Save", "Cancel", "Remove", "Move pending message down"]) {
      expect(controls.getByRole("button", { name })).toBeDisabled();
    }
    expect(
      controls.getByRole("button", { name: "Move pending message up" }),
    ).toBeDisabled();
    /* the queue itself does not react to the in-flight mutation */
    expect(rows()).toEqual([
      expect.stringContaining("message a"),
      expect.stringContaining("message b"),
    ]);

    gate.resolve();
    await waitFor(() =>
      expect(
        screen.queryByRole("textbox", { name: "Edit pending message" }),
      ).toBeNull(),
    );
  });

  it("removes no row optimistically; a refreshed snapshot removes it", async () => {
    const gate = deferred();
    const onEdit = vi.fn().mockReturnValue(gate.promise);
    const view = render(
      <QueuedInputs
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("b")]}
        onEdit={onEdit}
      />,
    );
    fireEvent.click(within(rowFor("b")).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(onEdit).toHaveBeenCalled());
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-b"],
      selectedRequestDocIds: ["doc-b"],
      messages: [],
    });
    /* still both rows, in the observed order, in flight and after the mutation */
    expect(rows()).toEqual([
      expect.stringContaining("message a"),
      expect.stringContaining("message b"),
    ]);
    gate.resolve();
    await waitFor(() =>
      expect(within(rowFor("a")).getByRole("button", { name: "Remove" })).toBeEnabled(),
    );
    expect(rows()).toEqual([
      expect.stringContaining("message a"),
      expect.stringContaining("message b"),
    ]);

    /* the refreshed session snapshot no longer lists the removed source */
    view.rerender(
      <QueuedInputs queued={[turn(entry("a"))]} queue={[entry("a")]} onEdit={onEdit} />,
    );
    expect(rows()).toEqual([expect.stringContaining("message a")]);
    expect(screen.queryByText("message b")).toBeNull();
  });

  it("reorders nothing optimistically; a refreshed snapshot shows the new order", async () => {
    const gate = deferred();
    const onEdit = vi.fn().mockReturnValue(gate.promise);
    const view = render(
      <QueuedInputs
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("b")]}
        onEdit={onEdit}
      />,
    );
    fireEvent.click(
      within(rowFor("a")).getByRole("button", {
        name: "Move pending message down",
      }),
    );
    await waitFor(() => expect(onEdit).toHaveBeenCalled());
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-b"],
      selectedRequestDocIds: ["doc-a", "doc-b"],
      messages: [
        { requestDocId: "doc-b", content: "message b" },
        { requestDocId: "doc-a", content: "message a" },
      ],
    });
    /* observed order holds until the refreshed snapshot arrives */
    expect(rows()).toEqual([
      expect.stringContaining("message a"),
      expect.stringContaining("message b"),
    ]);
    gate.resolve();
    await waitFor(() =>
      expect(within(rowFor("a")).getByRole("button", { name: "Remove" })).toBeEnabled(),
    );
    expect(rows()).toEqual([
      expect.stringContaining("message a"),
      expect.stringContaining("message b"),
    ]);

    view.rerender(
      <QueuedInputs
        queued={[turn(entry("b")), turn(entry("a"))]}
        queue={[entry("b"), entry("a")]}
        onEdit={onEdit}
      />,
    );
    expect(rows()).toEqual([
      expect.stringContaining("message b"),
      expect.stringContaining("message a"),
    ]);
  });

  it("cannot cross the cross-requester barrier, before or after a failed mutation", async () => {
    const onEdit = vi
      .fn()
      .mockRejectedValueOnce(new Error("mutation failed"))
      .mockResolvedValue(undefined);
    render(
      <QueuedInputs
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("barrier", null), entry("b")]}
        onEdit={onEdit}
      />,
    );
    const before = within(rowFor("a"));
    const after = within(rowFor("b"));
    expect(
      before.getByRole("button", { name: "Move pending message down" }),
    ).toBeDisabled();
    expect(
      after.getByRole("button", { name: "Move pending message up" }),
    ).toBeDisabled();

    /* a failed save still fences over the barrier's document */
    fireEvent.click(before.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Edit pending message" }), {
      target: { value: "corrected instruction" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Save" })).toBeEnabled(),
    );
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-barrier", "doc-b"],
      selectedRequestDocIds: ["doc-a"],
      messages: [{ requestDocId: "doc-a", content: "corrected instruction" }],
    });
    expect(rows()).toEqual([
      expect.stringContaining("message a"),
      expect.stringContaining("message b"),
    ]);
    expect(
      before.getByRole("button", { name: "Move pending message down" }),
    ).toBeDisabled();
    expect(
      after.getByRole("button", { name: "Move pending message up" }),
    ).toBeDisabled();
  });
});
