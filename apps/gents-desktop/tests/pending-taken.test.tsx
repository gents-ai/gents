import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { PendingTray } from "../src/ui/screens/PendingTray";

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

afterEach(() => vi.useRealTimers());

describe("a message the agent takes", () => {
  it("shows as picked up once the runtime stops listing it", () => {
    const queue = [entry("a"), entry("b")];
    const onEdit = vi.fn();
    const view = render(
      <PendingTray queued={queue.map(turn)} queue={queue} onEdit={onEdit} />,
    );
    view.rerender(
      <PendingTray queued={queue.map(turn)} queue={[entry("b")]} onEdit={onEdit} />,
    );
    const [first, second] = screen.getAllByTestId("queued-input");
    expect(within(first!).getByText("Picked up")).toBeInTheDocument();
    expect(within(first!).queryByRole("button")).toBeNull();
    expect(within(second!).getByRole("button", { name: "Remove" })).toBeInTheDocument();
  });

  it("is not picked up when the runtime never listed it", () => {
    const queue = [entry("b")];
    render(<PendingTray queued={[turn(entry("a")), turn(entry("b"))]} queue={queue} />);
    expect(screen.queryByText("Picked up")).toBeNull();
  });

  it("folds out of the tray when it leaves, then is gone", () => {
    vi.useFakeTimers();
    const queue = [entry("a"), entry("b")];
    const view = render(<PendingTray queued={queue.map(turn)} queue={queue} />);
    view.rerender(<PendingTray queued={[turn(entry("b"))]} queue={[entry("b")]} />);
    expect(screen.getByTestId("leaving-input")).toHaveTextContent("message a");
    expect(screen.getAllByTestId("queued-input")).toHaveLength(1);
    act(() => vi.advanceTimersByTime(250));
    expect(screen.queryByTestId("leaving-input")).toBeNull();
  });

  it("keeps the tray until the last message has folded away", () => {
    vi.useFakeTimers();
    const queue = [entry("a")];
    const view = render(<PendingTray queued={queue.map(turn)} queue={queue} />);
    view.rerender(<PendingTray queued={[]} queue={[]} />);
    expect(screen.getByTestId("queued-inputs")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(250));
    expect(screen.queryByTestId("queued-inputs")).toBeNull();
  });

  it("does not fold away a message another takes the place of", () => {
    const view = render(
      <PendingTray queued={[turn(entry("a"))]} queue={[entry("a")]} />,
    );
    view.rerender(<PendingTray queued={[turn(entry("a2"))]} queue={[entry("a2")]} />);
    expect(screen.queryByTestId("leaving-input")).toBeNull();
  });

  it("does not fold away a message the person removed", async () => {
    const queue = [entry("a"), entry("b")];
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const view = render(
      <PendingTray queued={queue.map(turn)} queue={queue} onEdit={onEdit} />,
    );
    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[0]!);
    await waitFor(() => expect(onEdit).toHaveBeenCalledTimes(1));
    view.rerender(
      <PendingTray queued={[turn(entry("b"))]} queue={[entry("b")]} onEdit={onEdit} />,
    );
    expect(screen.queryByTestId("leaving-input")).toBeNull();
    expect(screen.queryByText("Picked up")).toBeNull();
  });
});
