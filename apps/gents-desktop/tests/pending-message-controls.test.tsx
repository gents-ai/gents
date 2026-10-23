import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { PendingTray } from "../src/ui/screens/PendingTray";
import { moveDisabled, moveItem } from "./pending-tray-menu";

const entry = (id: string, group: string | null = "same"): PendingQueueEntryView => ({
  requestDocId: `doc-${id}`,
  requestId: id,
  content: `message ${id}`,
  editable: group !== null,
  steerable: group !== null,
  editGroup: group,
});
const turn = (row: PendingQueueEntryView, createdAt: string): PendingTurnView => ({
  requestId: row.requestId,
  content: row.content,
  selectedSkillIds: [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin: null,
  createdAt,
});

/** The native snapshot supplies arrival order independently of creation time. */
function setup(
  queue: PendingQueueEntryView[],
  queued = queue.map((row, index) => turn(row, `2026-10-10T00:00:0${index}Z`)),
) {
  const onEdit = vi.fn().mockResolvedValue(undefined);
  render(<PendingTray queued={queued} queue={queue} onEdit={onEdit} />);
  return onEdit;
}

describe("pending message controls", () => {
  it("hands a message to the composer to edit, and holds still while it is there", async () => {
    const onBeginEdit = vi.fn();
    const queue = [entry("a"), entry("barrier", null), entry("b")];
    const view = render(
      <PendingTray
        queued={queue.map((row) => turn(row, "2026-10-10T00:00:00Z"))}
        queue={queue}
        onEdit={vi.fn()}
        onBeginEdit={onBeginEdit}
      />,
    );
    fireEvent.click(screen.getAllByRole("button", { name: "Edit" })[0]!);
    expect(onBeginEdit).toHaveBeenCalledWith(entry("a"));
    view.rerender(
      <PendingTray
        queued={queue.map((row) => turn(row, "2026-10-10T00:00:00Z"))}
        queue={queue}
        onEdit={vi.fn()}
        editingDocId="doc-a"
        onBeginEdit={onBeginEdit}
      />,
    );
    expect(screen.getAllByTestId("queued-input")[0]).toHaveTextContent("Editing…");
    for (const button of screen.getAllByRole("button", { name: "Remove" }))
      expect(button).toBeDisabled();
    for (const grip of screen.getAllByTestId("pending-grip"))
      expect(grip.dataset.draggable).toBeUndefined();
  });

  it("offers no Edit without a composer to edit in", () => {
    setup([entry("a")]);
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
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
    fireEvent.click(await moveItem(screen.getAllByTestId("queued-input")[0]!, "down"));
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

  it("renders an edited message in its runtime slot, not by its newer creation time", async () => {
    const edited = { ...entry("edited"), content: "edited first" };
    const second = entry("second");
    const onEdit = setup(
      [edited, second],
      [turn(edited, "2026-10-10T00:00:02Z"), turn(second, "2026-10-10T00:00:01Z")],
    );
    expect(screen.getAllByTestId("queued-input").map((row) => row.textContent)).toEqual(
      [
        expect.stringContaining("edited first"),
        expect.stringContaining("message second"),
      ],
    );
    fireEvent.click(await moveItem(screen.getAllByTestId("queued-input")[0]!, "down"));
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith({
        expectedRequestDocIds: ["doc-edited", "doc-second"],
        selectedRequestDocIds: ["doc-edited", "doc-second"],
        messages: [
          { requestDocId: "doc-second", content: "message second" },
          { requestDocId: "doc-edited", content: "edited first" },
        ],
      }),
    );
  });

  it.each(["partially", "fully"])(
    "keeps native order when the queue was %s taken",
    (taken) => {
      const edited = entry("edited");
      const second = entry("second");
      setup(taken === "partially" ? [second] : [], [
        turn(edited, "2026-10-10T00:00:02Z"),
        turn(second, "2026-10-10T00:00:01Z"),
      ]);
      expect(
        screen.getAllByTestId("queued-input").map((row) => row.textContent),
      ).toEqual([
        expect.stringContaining("message edited"),
        expect.stringContaining("message second"),
      ]);
    },
  );

  it("cannot move a source across a different execution group", async () => {
    setup([entry("a"), entry("b", "other")]);
    const [a, b] = screen.getAllByTestId("queued-input");
    expect(await moveDisabled(a!, "down")).toBe(true);
    expect(await moveDisabled(b!, "up")).toBe(true);
    for (const grip of screen.getAllByTestId("pending-grip"))
      expect(grip.dataset.draggable).toBeUndefined();
    await moveItem(a!, "down");
    expect(
      screen.getByText(
        "Messages sent with different settings can’t be reordered together.",
      ),
    ).toBeInTheDocument();
  });

  it("steers a message with what is queued before it, and offers no steer past other settings", async () => {
    const queue = [entry("a"), entry("b"), entry("c", "other")];
    const onSteer = vi.fn().mockResolvedValue(undefined);
    render(
      <PendingTray
        queued={queue.map((row, index) => turn(row, `2026-10-10T00:00:0${index}Z`))}
        queue={queue}
        onEdit={vi.fn()}
        onSteer={onSteer}
      />,
    );
    const steer = screen.getAllByRole("button", { name: "Steer" });
    expect(steer).toHaveLength(2);
    fireEvent.click(steer[1]!);
    await waitFor(() =>
      expect(onSteer).toHaveBeenCalledWith({
        expectedRequestDocIds: ["doc-a", "doc-b", "doc-c"],
        requestDocId: "doc-b",
      }),
    );
  });

  it("says on hover how each queued message will be sent", () => {
    setup([{ ...entry("b"), steerable: false }, entry("a")]);
    expect(screen.getByText("message a")).toHaveAttribute(
      "title",
      "Sends after the running turn finishes",
    );
    expect(screen.getByText("message b")).toHaveAttribute(
      "title",
      "Steers in at the next step",
    );
  });

  it("says a steer behind a queued message waits for it", () => {
    const steer = (id: string) => ({ ...entry(id), steerable: false });
    setup([steer("a"), entry("b"), steer("c")]);
    expect(screen.getByText("message a")).toHaveAttribute(
      "title",
      "Steers in at the next step",
    );
    expect(screen.getByText("message c")).toHaveAttribute(
      "title",
      "Waits for the queued message before it",
    );
  });

  it("offers no steer without a running session to steer into", () => {
    setup([entry("a")]);
    expect(screen.queryByRole("button", { name: "Steer" })).toBeNull();
  });
});
