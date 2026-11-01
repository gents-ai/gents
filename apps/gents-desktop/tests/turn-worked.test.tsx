import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it, vi } from "vitest";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import { LONG_TURN_MS, turnsWorked, turnWorked } from "@/lib/turnWorked";
import { groupTranscript } from "@/screens/transcript-groups";
import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { NO_WORKERS } from "../src/ui/screens/workers";
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderIn, testApp } from "./app-fixture";
import { assistantMessage, sessionSnapshot, userMessage } from "./timeline-fixture";

const T0 = Date.parse("2026-10-31T12:00:00Z");
const after = (ms: number) => new Date(T0 + ms).toISOString();

let seq = 0;
const call = (startedAt: string | null, completedAt: string | null = startedAt) =>
  ({
    itemKey: `t${++seq}`,
    toolName: "read_file",
    statusKind: "success",
    startedAt,
    completedAt,
    presentation: {
      kind: "fileRead",
      operation: "read_file",
      target: "src/a.rs",
      returnedCount: null,
      totalCount: null,
      truncated: false,
      body: "",
      fallbackOutput: null,
    },
    reconstruction: { state: "ready" },
  }) as RenderedToolCallView;

const group = (n: number, ...tools: RenderedToolCallView[]): RenderedTimelineItem => ({
  kind: "toolGroup",
  itemKey: `g${n}`,
  messageSequence: n,
  tools,
});
const person = (n: number, timestamp: string | null = null) =>
  userMessage({ itemKey: `u${n}`, sequence: n, content: "go", timestamp });
const said = (n: number, timestamp: string | null) =>
  assistantMessage({ itemKey: `a${n}`, sequence: n, content: "Done.", timestamp });

describe("how long a turn worked, from its rows", () => {
  it("runs from its first call's start to its final reply", () => {
    const tools = [call(after(2_000), after(9_000)), call(after(1_000), after(4_000))];
    expect(turnWorked(tools, after(12_000))).toBe(11_000);
  });

  it("is not told without a call that started or a reply with a time", () => {
    expect(turnWorked([], after(1_000))).toBeNull();
    expect(turnWorked([call(null), call("not a time")], after(1_000))).toBeNull();
    expect(turnWorked([call(after(0))], null)).toBeNull();
    expect(turnWorked([call(after(0))], "not a time")).toBeNull();
  });

  it("is not told when its times contradict each other or any turn", () => {
    expect(turnWorked([call(after(5_000))], after(1_000))).toBeNull();
    expect(turnWorked([call(after(0)), call(after(5_000))], after(1_000))).toBeNull();
    expect(turnWorked([call(after(0))], after(25 * 3_600_000))).toBeNull();
    expect(turnWorked([call("1970-01-01T00:00:00Z")], after(0))).toBeNull();
  });

  it("keeps each finished turn's own calls, and leaves a running turn out", () => {
    const entries = groupTranscript([
      person(1, after(0)),
      group(2, call(after(1_000))),
      said(3, after(4_000)),
      person(4, after(5_000)),
      group(5, call(after(6_000))),
      said(6, after(90_000)),
      person(7, after(91_000)),
      group(8, call(after(92_000))),
    ]);
    const worked = turnsWorked(entries, new Set(["a3", "a6"]));
    expect(Object.fromEntries(worked)).toEqual({ a3: 3_000, a6: 84_000 });
  });
});

describe("how long a turn worked, on its reply's time", () => {
  const render = (workedMs: number) => {
    const replied = after(1_000 + workedMs);
    const session = sessionSnapshot({
      timelineItems: [
        person(1, after(0)),
        group(2, call(after(1_000), after(1_500))),
        said(3, replied),
      ],
    });
    const { container } = renderIn(
      testApp(),
      <TranscriptPanel
        inFlight={false}
        scroller={null}
        session={session}
        workers={NO_WORKERS}
        parentWork={NO_PARENT}
        workerActions={{ interrupt: () => {} }}
      />,
    );
    const time = [...container.querySelectorAll("time")].find(
      (node) => node.dateTime === replied,
    )!;
    return { time, meta: time.closest('[data-slot="message-meta"]')! };
  };

  it("says a short turn's time only over the reply's time", async () => {
    const { time, meta } = render(3_000);
    expect(meta.textContent).toBe(time.textContent);
    expect(await hovered(time)).toBe(
      `${new Date(time.dateTime).toLocaleString()} · Worked for 3s`,
    );
  });

  it("says a turn over a minute beside the reply's time too", async () => {
    const { time, meta } = render(4 * 60_000 + 12_000);
    expect(await hovered(time)).toMatch(/ · Worked for 4m 12s$/);
    expect(meta.textContent).toBe(`${time.textContent} · Worked for 4m 12s`);
  });

  it("keeps a turn of exactly a minute off the row", async () => {
    const { time, meta } = render(LONG_TURN_MS);
    expect(await hovered(time)).toMatch(/ · Worked for 1m$/);
    expect(meta.textContent).toBe(time.textContent);
  });
});

/* the app's tooltip over an element, once hovering opens it */
async function hovered(element: Element) {
  await userEvent.hover(element);
  return (await screen.findByText(/Worked for/, {}, { timeout: 2000 })).textContent;
}
