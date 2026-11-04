import { act, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  closeFind,
  findMatches,
  openFind,
  type FindMatch,
} from "@/screens/session/find";
import { FindBar, redrawn } from "@/screens/session/FindBar";
import { IS_MAC } from "@/screens/shortcutKeys";
import { useGroupState } from "@/screens/ActivityGroup";
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
const NO_ROWS = {
  folded: {},
  openStep: {},
  reasoning: {},
  setFolded() {},
  setOpenStep() {},
  setReasoning() {},
};

afterEach(() => act(() => closeFind()));

describe("finding in a session", () => {
  it("finds every occurrence in the rows' data, with what opens it", () => {
    const { matches: found, more } = findMatches(entries, keyOf, "SNAKE");
    expect(more).toBe(false);
    expect(found.map((m) => m.rowKey)).toEqual(["u1", "g1", "g1", "a1", "a1"]);
    /* the output is drawn once its step opens; the thought, once its reasoning does */
    expect(found[1]).toMatchObject({ group: { key: "g1", step: "t1" } });
    expect(found[2]).toMatchObject({ group: { key: "g1" }, reasoning: "a0" });
    /* counted in the part the page draws it in: a step's output and a
       reasoning each on their own, as only an opened one is drawn */
    expect(found.map((m) => m.nth)).toEqual([0, 0, 0, 0, 1]);
    expect(findMatches(entries, keyOf, "  ").matches).toEqual([]);
    /* past the limit: the newest ones, and more */
    const capped = findMatches(entries, keyOf, "snake", 2);
    expect(capped.more).toBe(true);
    expect(capped.matches.map((m) => [m.rowKey, m.nth])).toEqual([
      ["a1", 0],
      ["a1", 1],
    ]);
  });

  it("counts a step's output on its own, so a folded step before it doesn't shift it", () => {
    const steps = groupTranscript([
      {
        kind: "toolGroup",
        itemKey: "g2",
        messageSequence: null,
        tools: [command("s1", "snake one\nsnake two"), command("s2", "snake three")],
      },
    ]);
    const { matches: found } = findMatches(steps, keyOf, "snake");
    const inSecond = found.filter((m) => m.group?.step === "s2");
    /* only s2's output is drawn while s2 is open, so its match is its first */
    expect(inSecond.map((m) => m.nth)).toEqual([0]);
  });

  it("closes on Escape from anywhere, unless a field or menu takes it", () => {
    render(
      <>
        <button type="button">elsewhere</button>
        <textarea aria-label="draft" />
        <FindBar
          scroller={null}
          sessionKey="s1"
          entries={entries}
          keyOf={keyOf}
          goTo={() => {}}
          opener={NO_ROWS}
          older={{ more: false, loading: false, load: () => {}, messages: null }}
        />
      </>,
    );
    const open = () =>
      fireEvent.keyDown(window, { key: "f", [IS_MAC ? "metaKey" : "ctrlKey"]: true });
    const bar = () => screen.queryByRole("search", { name: "Find in session" });
    open();
    screen.getByRole("textbox", { name: "draft" }).focus();
    fireEvent.keyDown(screen.getByRole("textbox", { name: "draft" }), {
      key: "Escape",
    });
    expect(bar()).not.toBeNull();
    screen.getByRole("button", { name: "elsewhere" }).focus();
    fireEvent.keyDown(screen.getByRole("button", { name: "elsewhere" }), {
      key: "Escape",
    });
    expect(bar()).toBeNull();
  });

  it("starts from the text selected on the page", () => {
    render(
      <>
        <p data-testid="said">the snake game</p>
        <FindBar
          scroller={null}
          sessionKey="s1"
          entries={entries}
          keyOf={keyOf}
          goTo={() => {}}
          opener={NO_ROWS}
          older={{ more: false, loading: false, load: () => {}, messages: null }}
        />
      </>,
    );
    const said = screen.getByTestId("said").firstChild!;
    const range = document.createRange();
    range.setStart(said, 4);
    range.setEnd(said, 9);
    document.getSelection()!.removeAllRanges();
    document.getSelection()!.addRange(range);
    fireEvent.keyDown(window, { key: "f", [IS_MAC ? "metaKey" : "ctrlKey"]: true });
    expect(screen.getByRole("textbox", { name: "Find in session" })).toHaveValue(
      "snake",
    );
    document.getSelection()!.removeAllRanges();
  });

  it("opens on its key, counts, steps both ways around, and closes with Escape", async () => {
    vi.useFakeTimers();
    const goTo = vi.fn();
    const opener = {
      folded: {},
      openStep: {},
      reasoning: {},
      setFolded: vi.fn(),
      setOpenStep: vi.fn(),
      setReasoning: vi.fn(),
    };
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
          older={{ more: false, loading: false, load: () => {}, messages: null }}
        />
      </>,
    );
    screen.getByRole("button", { name: "before" }).focus();
    fireEvent.keyDown(window, { key: "f", [IS_MAC ? "metaKey" : "ctrlKey"]: true });
    const field = screen.getByRole("textbox", { name: "Find in session" });
    expect(field).toHaveFocus();
    expect(field).toHaveAttribute("autocorrect", "off");
    /* one character waits for Enter, since it matches nearly everything */
    fireEvent.change(field, { target: { value: "s" } });
    act(() => vi.advanceTimersByTime(200));
    expect(screen.getByText("Enter to find")).toBeInTheDocument();
    expect(screen.queryByText(/ of \d/)).toBeNull();
    fireEvent.keyDown(field, { key: "Enter" });
    act(() => vi.advanceTimersByTime(200));
    expect(screen.getByText(/^1 of \d+\+?$/)).toBeInTheDocument();

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
    /* off a Mac, F3 and Shift+F3 step as well */
    if (!IS_MAC) {
      fireEvent.keyDown(window, { key: "F3" });
      expect(screen.getByText("2 of 5")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "F3", shiftKey: true });
      expect(screen.getByText("1 of 5")).toBeInTheDocument();
    }

    fireEvent.change(field, { target: { value: "lizard" } });
    act(() => vi.advanceTimersByTime(200));
    expect(screen.getByText("No matches in the loaded messages")).toBeInTheDocument();

    fireEvent.keyDown(field, { key: "Escape" });
    expect(screen.queryByRole("search")).toBeNull();
    expect(screen.getByRole("button", { name: "before" })).toHaveFocus();
    vi.useRealTimers();
  });

  it("searches all: reads the older pages one at a time, counts them, and stops", () => {
    vi.useFakeTimers();
    /* three pages of two messages; only the newest is read at first */
    const pages = [
      [
        userMessage({
          itemKey: "o1",
          sequence: 0,
          content: "The oldest note says lizard.",
        }),
        assistantMessage({ itemKey: "o2", sequence: 1, content: "Noted." }),
      ],
      [
        userMessage({ itemKey: "m1", sequence: 2, content: "Middle." }),
        assistantMessage({ itemKey: "m2", sequence: 3, content: "Still middle." }),
      ],
      [
        userMessage({ itemKey: "n1", sequence: 4, content: "Newest." }),
        assistantMessage({ itemKey: "n2", sequence: 5, content: "A lizard again." }),
      ],
    ];
    const reads = vi.fn();
    function Session() {
      const [from, setFrom] = useState(2);
      const [loading, setLoading] = useState(false);
      const shown = groupTranscript(pages.slice(from).flat());
      return (
        <FindBar
          scroller={null}
          sessionKey="s1"
          entries={shown}
          keyOf={keyOf}
          goTo={() => {}}
          opener={NO_ROWS}
          older={{
            more: from > 0,
            loading,
            load: () => {
              reads();
              setLoading(true);
              setTimeout(() => {
                setFrom((f) => f - 1);
                setLoading(false);
              }, 100);
            },
            messages: { loaded: (3 - from) * 2, total: 6 },
          }}
        />
      );
    }
    render(<Session />);
    act(() => openFind());
    const field = screen.getByRole("textbox", { name: "Find in session" });
    fireEvent.change(field, { target: { value: "lizard" } });
    act(() => vi.advanceTimersByTime(200));
    expect(screen.getByText("1 of 1")).toBeInTheDocument();
    expect(screen.getByText("Searching loaded messages")).toBeInTheDocument();
    expect(reads).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Search all" }));
    expect(reads).toHaveBeenCalledTimes(1);
    expect(screen.getByText("Searched 2 of 6 messages")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(100));
    expect(screen.getByText("Searched 4 of 6 messages")).toBeInTheDocument();
    expect(reads).toHaveBeenCalledTimes(2);

    /* Stop leaves the page being read to land, and reads no more */
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    act(() => vi.advanceTimersByTime(500));
    expect(reads).toHaveBeenCalledTimes(2);
    /* that page held the oldest match: stepping reaches it */
    expect(screen.getByText(/of 2$/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Search all" })).toBeNull();
    vi.useRealTimers();
  });

  describe("what find opened", () => {
    /* two groups, each with a step whose output holds the word, and a reply */
    const rows = groupTranscript([
      userMessage({ itemKey: "u1", content: "Run both." }),
      {
        kind: "toolGroup",
        itemKey: "g1",
        messageSequence: null,
        tools: [command("t1", "a lizard")],
      },
      assistantMessage({ itemKey: "a1", content: "First done." }),
      {
        kind: "toolGroup",
        itemKey: "g2",
        messageSequence: null,
        tools: [command("t2", "a lizard")],
      },
      assistantMessage({ itemKey: "a2", content: "A lizard, twice." }),
    ]);
    let state!: ReturnType<typeof useGroupState>;
    function Session() {
      state = useGroupState("s1");
      const [scroller, setScroller] = useState<HTMLElement | null>(null);
      return (
        <div ref={setScroller}>
          <div data-transcript-list>
            {rows.map((entry) => (
              <div key={entry.key} data-row-key={entry.key}>
                <span>{entry.key}</span>
              </div>
            ))}
          </div>
          <FindBar
            scroller={scroller}
            sessionKey="s1"
            entries={rows}
            keyOf={keyOf}
            goTo={() => {}}
            opener={state}
            older={{ more: false, loading: false, load: () => {}, messages: null }}
          />
        </div>
      );
    }
    const findLizard = () => {
      render(<Session />);
      act(() => openFind());
      const field = screen.getByRole("textbox", { name: "Find in session" });
      fireEvent.change(field, { target: { value: "lizard" } });
      act(() => vi.advanceTimersByTime(200));
      return field;
    };
    afterEach(() => vi.useRealTimers());

    it("folds a group again once the match leaves it", () => {
      vi.useFakeTimers();
      const field = findLizard();
      expect(screen.getByText("1 of 3")).toBeInTheDocument();
      expect(state.folded.g1).toBe(false);
      expect(state.openStep.g1).toBe("t1");
      fireEvent.keyDown(field, { key: "Enter" });
      expect(state.folded.g2).toBe(false);
      expect(state.folded.g1).toBe(true);
      expect(state.openStep.g1).toBeNull();
    });

    it("leaves open a group the person clicked into", () => {
      vi.useFakeTimers();
      const field = findLizard();
      fireEvent.pointerDown(screen.getByText("g1"));
      fireEvent.keyDown(field, { key: "Enter" });
      expect(state.folded.g1).toBe(false);
      expect(state.openStep.g1).toBe("t1");
    });

    it("on Escape folds all it opened but the current match's", () => {
      vi.useFakeTimers();
      const field = findLizard();
      /* the second match, in the second group */
      fireEvent.keyDown(field, { key: "Enter" });
      expect(screen.getByText("2 of 3")).toBeInTheDocument();
      expect(state.folded.g2).toBe(false);
      fireEvent.keyDown(field, { key: "Escape" });
      expect(state.folded.g1).toBe(true);
      expect(state.folded.g2).toBe(false);
      expect(state.openStep.g2).toBe("t2");
    });

    it("holds the current match's row while what find opened folds", () => {
      vi.useFakeTimers();
      const held: string[] = [];
      document.addEventListener("transcript:hold", (e) =>
        held.push((e.target as HTMLElement).dataset.rowKey ?? ""),
      );
      const field = findLizard();
      fireEvent.keyDown(field, { key: "Enter" });
      expect(held).toContain("g2");
    });
  });
});

describe("counting a drawn part as the page draws it", () => {
  const m = (part: Partial<FindMatch>, nth: number): FindMatch => ({
    rowKey: "r1",
    nth,
    ...part,
  });
  const step = { group: { key: "g", step: "s1" } };
  const data = [m({}, 0), m(step, 0), m(step, 1), m(step, 2), { rowKey: "r2", nth: 0 }];

  it("takes a drawn part's count from the page, in the data's place", () => {
    const out = redrawn(data, { rowKey: "r1", counts: { "": 1, "step:s1": 2 } });
    expect(out.map((x) => [x.rowKey, x.group?.step ?? "", x.nth])).toEqual([
      ["r1", "", 0],
      ["r1", "s1", 0],
      ["r1", "s1", 1],
      ["r2", "", 0],
    ]);
  });

  it("keeps the data's matches for a part the page draws none of", () => {
    const out = redrawn(data, { rowKey: "r1", counts: { "": 1, "step:s1": 0 } });
    expect(out.filter((x) => x.group?.step === "s1")).toHaveLength(3);
  });
});
