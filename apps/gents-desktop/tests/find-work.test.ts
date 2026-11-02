import { afterEach, describe, expect, it, vi } from "vitest";

import { findMatches, startFind, type Found } from "@/screens/session/find";
import { groupTranscript, type TranscriptEntry } from "@/screens/transcript-groups";
import { command, harnessSession } from "./find-fixture";
import { assistantMessage, userMessage } from "./timeline-fixture";

const keyOf = (entry: TranscriptEntry) => entry.key;

afterEach(() => vi.restoreAllMocks());

/* what finding does, counted in rows, slices and calls: its time is
   measured apart, by `npm run perf:find` */
describe("the work finding does", () => {
  it("yields between slices and resumes where it stopped, newest rows first", () => {
    const rows = harnessSession(40);
    /* each reading of the clock moves it a millisecond, so a 6ms slice
       ends after a few rows whatever the machine */
    let now = 0;
    vi.spyOn(performance, "now").mockImplementation(() => (now += 1));
    const queued: (() => void)[] = [];
    vi.spyOn(window, "setTimeout").mockImplementation(((run: () => void) => {
      queued.push(run);
      return 0;
    }) as unknown as typeof window.setTimeout);
    const heard: Found[] = [];
    startFind(rows, keyOf, "fixture row", (found) => heard.push(found));
    expect(heard).toHaveLength(1);
    expect(heard[0]!.done).toBe(false);
    /* the first slice holds the newest rows */
    expect(heard[0]!.matches.at(-1)!.rowKey).toBe(rows.at(-1)!.key);
    while (queued.length) queued.shift()!();
    expect(heard.length).toBeGreaterThan(2);
    expect(heard.slice(0, -1).every((found) => !found.done)).toBe(true);
    expect(heard.at(-1)!.done).toBe(true);
    /* each slice adds older rows to what the one before found */
    const sizes = heard.map((found) => found.matches.length);
    expect(sizes).toEqual([...sizes].sort((a, b) => a - b));
    expect(heard.at(-1)!.matches).toEqual(
      findMatches(rows, keyOf, "fixture row").matches,
    );
  });

  it("stops at its limit, keeping the newest matches in drawing order", () => {
    const rows = harnessSession(40);
    const { matches, more } = findMatches(rows, keyOf, "fixture row", 5);
    expect(more).toBe(true);
    expect(matches.map((m) => m.rowKey)).toEqual(rows.slice(-5).map((r) => r.key));
    /* rows past the limit aren't searched */
    const searched = vi.fn(keyOf);
    findMatches(rows, searched, "fixture row", 5);
    expect(searched.mock.calls.length).toBeLessThan(rows.length);
  });

  it("lowercases each row's text once, and reuses it for the next query", () => {
    const rows = groupTranscript([
      userMessage({ itemKey: "u1", content: "Where is the Snake game?" }),
      {
        kind: "toolGroup",
        itemKey: "g1",
        messageSequence: null,
        tools: [command("t1", "snake.py\nREADME.md")],
      },
      assistantMessage({ itemKey: "a1", content: "The snake game is in snake.py." }),
    ]);
    const lower = vi.spyOn(String.prototype, "toLowerCase");
    findMatches(rows, keyOf, "snake");
    const first = lower.mock.calls.length;
    lower.mockClear();
    findMatches(rows, keyOf, "game");
    /* the query, and nothing of the rows */
    expect(lower.mock.calls.length).toBe(1);
    expect(first).toBeGreaterThan(1);
  });
});
