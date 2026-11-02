import { act, fireEvent, render, screen } from "@testing-library/react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { closeFind, findMatches, openFind } from "@/screens/session/find";
import { FindBar } from "@/screens/session/FindBar";
import { IS_MAC } from "@/screens/shortcutKeys";
import { groupTranscript, type TranscriptEntry } from "@/screens/transcript-groups";
import { assistantMessage, userMessage } from "./timeline-fixture";

const command = (itemKey: string, stdout: string): RenderedToolCallView =>
  ({
    itemKey,
    toolName: "command",
    statusKind: "success",
    presentation: {
      kind: "command",
      command: "ls",
      exitCode: 0,
      timedOut: false,
      failed: false,
      durationMs: 1,
      cwd: "~",
      executionMode: "sandboxed",
      networkMode: "disabled",
      stdout,
      stderr: "",
      fallbackOutput: null,
    },
  }) as unknown as RenderedToolCallView;

const entries: TranscriptEntry[] = groupTranscript([
  userMessage({ itemKey: "u1", content: "Where is the Snake game?" }),
  {
    kind: "toolGroup",
    itemKey: "g1",
    messageSequence: null,
    tools: [command("t1", "snake.py\nREADME.md")],
  },
  assistantMessage({ itemKey: "a0", reasoning: "Look for snake files first." }),
  assistantMessage({ itemKey: "a1", content: "The snake game is in snake.py." }),
]);
const keyOf = (entry: TranscriptEntry) => entry.key;

afterEach(() => act(() => closeFind()));

describe("finding in a session", () => {
  it("finds every occurrence in the rows' data, with what opens it", () => {
    const found = findMatches(entries, keyOf, "SNAKE");
    expect(found.map((m) => m.rowKey)).toEqual(["u1", "g1", "g1", "a1", "a1"]);
    /* the output is drawn once its step opens; the thought, once its reasoning does */
    expect(found[1]).toMatchObject({ group: { key: "g1", step: "t1" } });
    expect(found[2]).toMatchObject({ group: { key: "g1" }, reasoning: "a0" });
    expect(found.map((m) => m.nth)).toEqual([0, 0, 1, 0, 1]);
    expect(findMatches(entries, keyOf, "  ")).toEqual([]);
  });

  it("opens on its key, counts, steps both ways around, and closes with Escape", async () => {
    vi.useFakeTimers();
    const goTo = vi.fn();
    const opener = { setFolded: vi.fn(), setOpenStep: vi.fn(), setReasoning: vi.fn() };
    render(
      <>
        <button type="button">before</button>
        <FindBar
          scroller={null}
          sessionKey="s1"
          entries={entries}
          keyOf={keyOf}
          goTo={goTo}
          opener={opener}
          older={{ more: false, loading: false, load: () => {} }}
        />
      </>,
    );
    screen.getByRole("button", { name: "before" }).focus();
    fireEvent.keyDown(window, { key: "f", [IS_MAC ? "metaKey" : "ctrlKey"]: true });
    const field = screen.getByRole("textbox", { name: "Find in session" });
    expect(field).toHaveFocus();
    expect(field).toHaveAttribute("autocorrect", "off");

    fireEvent.change(field, { target: { value: "snake" } });
    act(() => vi.advanceTimersByTime(200));
    expect(screen.getByText("1 of 5")).toBeInTheDocument();
    expect(goTo).toHaveBeenLastCalledWith({ key: "u1", offset: 96 });

    fireEvent.keyDown(field, { key: "Enter" });
    expect(screen.getByText("2 of 5")).toBeInTheDocument();
    expect(opener.setFolded).toHaveBeenCalledWith("g1", false);
    expect(opener.setOpenStep).toHaveBeenCalledWith("g1", "t1");

    fireEvent.keyDown(field, { key: "Enter", shiftKey: true });
    fireEvent.keyDown(field, { key: "Enter", shiftKey: true });
    expect(screen.getByText("5 of 5")).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "g", [IS_MAC ? "metaKey" : "ctrlKey"]: true });
    expect(screen.getByText("1 of 5")).toBeInTheDocument();

    fireEvent.change(field, { target: { value: "lizard" } });
    act(() => vi.advanceTimersByTime(200));
    expect(screen.getByText("No matches in the loaded messages")).toBeInTheDocument();

    fireEvent.keyDown(field, { key: "Escape" });
    expect(screen.queryByRole("search")).toBeNull();
    expect(screen.getByRole("button", { name: "before" })).toHaveFocus();
    vi.useRealTimers();
  });

  it("says it searched only the loaded messages, and loads older ones", () => {
    const load = vi.fn();
    render(
      <FindBar
        scroller={null}
        sessionKey="s1"
        entries={entries}
        keyOf={keyOf}
        goTo={() => {}}
        opener={{ setFolded() {}, setOpenStep() {}, setReasoning() {} }}
        older={{ more: true, loading: false, load }}
      />,
    );
    act(() => openFind());
    expect(screen.getByText(/Searching loaded messages/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Load older" }));
    expect(load).toHaveBeenCalled();
  });
});
