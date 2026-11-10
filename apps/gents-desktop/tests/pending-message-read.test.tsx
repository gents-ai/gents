import { fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { PendingTray } from "../src/ui/screens/PendingTray";
import { renderIn, testApp } from "./app-fixture";

const LONG = "first line of a long queued message\nsecond line\nthird line";

const entry = (
  id: string,
  content: string,
  skills: string[] = [],
): PendingQueueEntryView => ({
  requestDocId: `doc-${id}`,
  requestId: id,
  content,
  selectedSkillIds: skills,
  editable: true,
  steerable: true,
  editGroup: "same",
});
const turn = (row: PendingQueueEntryView): PendingTurnView => ({
  requestId: row.requestId,
  content: row.content,
  selectedSkillIds: row.selectedSkillIds ?? [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin: null,
  createdAt: "2026-10-10T00:00:00Z",
});

function setup(queue: PendingQueueEntryView[]) {
  const onEdit = vi.fn().mockResolvedValue(undefined);
  const onBeginEdit = vi.fn();
  renderIn(
    testApp(),
    <PendingTray
      queued={queue.map(turn)}
      queue={queue}
      onEdit={onEdit}
      onBeginEdit={onBeginEdit}
    />,
  );
  return { onEdit, onBeginEdit };
}

const toggles = () => screen.getAllByRole("button", { name: "Show whole message" });
const texts = () => screen.getAllByTestId("queued-text");

afterEach(() => {
  Object.assign(navigator, { clipboard: undefined });
});

describe("reading a queued message", () => {
  it("expands in place from its toggle and collapses on a second press", () => {
    setup([entry("a", LONG), entry("b", "short")]);
    const [toggle] = toggles();
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(texts()[0]).toHaveClass("truncate");

    fireEvent.click(toggle!);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(texts()[0]!.dataset.expanded).toBe("true");
    expect(texts()[0]).not.toHaveClass("truncate");
    expect(texts()[0]).toHaveClass(
      "max-h-48",
      "overflow-y-auto",
      "whitespace-pre-wrap",
    );
    expect(texts()[0]!.textContent).toBe(LONG);
    expect(texts()[1]!.dataset.expanded).toBeUndefined();

    fireEvent.click(toggle!);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(texts()[0]).toHaveClass("truncate");
  });

  it("offers nothing more for a message one line holds", () => {
    setup([entry("a", "short")]);
    expect(screen.queryByRole("button", { name: "Show whole message" })).toBeNull();
    fireEvent.click(texts()[0]!);
    expect(texts()[0]!.dataset.expanded).toBeUndefined();
  });

  it("offers the whole message when one line cuts it off", () => {
    const width = vi
      .spyOn(HTMLElement.prototype, "scrollWidth", "get")
      .mockReturnValue(500);
    setup([entry("a", "a long message on one line")]);
    expect(toggles()).toHaveLength(1);
    width.mockRestore();
  });

  it("expands on a click on the message and collapses on the next", () => {
    setup([entry("a", LONG)]);
    fireEvent.click(texts()[0]!);
    expect(toggles()[0]).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(texts()[0]!);
    expect(toggles()[0]).toHaveAttribute("aria-expanded", "false");
  });

  it("collapses on Escape", () => {
    setup([entry("a", LONG)]);
    fireEvent.click(toggles()[0]!);
    fireEvent.keyDown(toggles()[0]!, { key: "Escape" });
    expect(toggles()[0]).toHaveAttribute("aria-expanded", "false");
  });

  it("shows one message whole at a time", () => {
    setup([entry("a", LONG), entry("b", LONG)]);
    fireEvent.click(toggles()[0]!);
    fireEvent.click(toggles()[1]!);
    expect(toggles().map((t) => t.getAttribute("aria-expanded"))).toEqual([
      "false",
      "true",
    ]);
  });

  it("copies the whole text with its skills, as a sent message copies", () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    setup([entry("a", LONG, ["review"])]);
    expect(screen.queryByRole("button", { name: "Copy" })).toBeNull();
    fireEvent.click(toggles()[0]!);
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith(`/review ${LONG}`);
    expect(toggles()[0]).toHaveAttribute("aria-expanded", "true");
  });

  it("keeps editing and removing while it is open", () => {
    const { onBeginEdit } = setup([entry("a", LONG)]);
    fireEvent.click(toggles()[0]!);
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    expect(onBeginEdit).toHaveBeenCalledWith(entry("a", LONG));
    expect(screen.getByRole("button", { name: "Remove" })).toBeEnabled();
    expect(toggles()[0]).toHaveAttribute("aria-expanded", "true");
  });
});
