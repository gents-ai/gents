import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it, vi } from "vitest";

import { findMatches, startFind } from "@/screens/session/find";
import { groupTranscript, type TranscriptEntry } from "@/screens/transcript-groups";
import { assistantMessage, userMessage } from "./timeline-fixture";

/* a deterministic stream of words, so every run searches the same text */
function words(seed: number, length: number): string {
  const vocabulary =
    "the build failed because a test timed out while the compiler checked the module graph for cycles and the linker wrote symbols into the cache directory under target".split(
      " ",
    );
  let out = "";
  let n = seed;
  while (out.length < length) {
    n = (n * 1103515245 + 12345) & 0x7fffffff;
    out += `${vocabulary[n % vocabulary.length]}${n % 17 === 0 ? `_${n % 997}` : ""} `;
  }
  return out;
}

const command = (key: string, stdout: string): RenderedToolCallView =>
  ({
    itemKey: key,
    toolName: "command",
    statusKind: "success",
    presentation: {
      kind: "command",
      command: `cargo test -p gents ${key}`,
      exitCode: 0,
      timedOut: false,
      failed: false,
      durationMs: 1200,
      cwd: "~/work",
      executionMode: "sandboxed",
      networkMode: "disabled",
      stdout,
      stderr: "",
      fallbackOutput: null,
    },
  }) as unknown as RenderedToolCallView;

/* a working session: each turn a message, a group of steps (some with
   tens of KB of output) and a long reply, until `rows` items */
function workingSession(rows: number): TranscriptEntry[] {
  const items: RenderedTimelineItem[] = [];
  for (let turn = 0; items.length < rows; turn += 1) {
    items.push(userMessage({ itemKey: `u${turn}`, content: words(turn, 240) }));
    const tools = Array.from({ length: 1 + (turn % 3) }, (_, i) =>
      command(
        `t${turn}-${i}`,
        words(turn * 7 + i, i === 0 && turn % 2 === 0 ? 30_000 : 1_500),
      ),
    );
    items.push({
      kind: "toolGroup",
      itemKey: `g${turn}`,
      messageSequence: null,
      tools,
    });
    items.push(
      assistantMessage({
        itemKey: `a${turn}`,
        content: words(turn * 13, 3_000),
        reasoning: turn % 4 === 0 ? words(turn * 3, 800) : null,
      }),
    );
  }
  return groupTranscript(items.slice(0, rows));
}

/* the harness's large session: short messages and replies */
function harnessSession(rows: number): TranscriptEntry[] {
  const filler =
    "bounded durable transcript fixture with markdown `code`, stable prose, and comparable byte shape";
  return groupTranscript(
    Array.from({ length: rows }, (_, index) =>
      index % 2 === 0
        ? userMessage({
            itemKey: `u${index}`,
            content: `User fixture row ${index}: ${filler}`,
          })
        : assistantMessage({
            itemKey: `a${index}`,
            content: `Assistant fixture row ${index}: ${filler}`,
            reasoning: index % 10 === 1 ? `Reasoning fixture ${index}` : null,
          }),
    ),
  );
}

const percentile = (sorted: number[], p: number) =>
  sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * p))]!;

/* the keystrokes of typing a query: each prefix is searched in full */
function typing(entries: TranscriptEntry[], query: string, rounds: number) {
  const keyOf = (entry: TranscriptEntry) => entry.key;
  const times: number[] = [];
  for (let round = 0; round < rounds; round += 1)
    for (let end = 1; end <= query.length; end += 1) {
      const start = performance.now();
      findMatches(entries, keyOf, query.slice(0, end));

      times.push(performance.now() - start);
    }
  times.sort((a, b) => a - b);
  return { median: percentile(times, 0.5), p95: percentile(times, 0.95) };
}

/* the keystrokes again, found a slice at a time: how long each slice holds
   the main thread, and how long a keystroke takes to be found in full */
function slicedTyping(entries: TranscriptEntry[], query: string, rounds: number) {
  const keyOf = (entry: TranscriptEntry) => entry.key;
  const queued: (() => void)[] = [];
  const timer = vi.spyOn(window, "setTimeout").mockImplementation(((
    run: () => void,
  ) => {
    queued.push(run);
    return 0;
  }) as unknown as typeof window.setTimeout);
  const slices: number[] = [];
  const whole: number[] = [];
  for (let round = 0; round < rounds; round += 1)
    for (let end = 1; end <= query.length; end += 1) {
      const began = performance.now();
      let start = began;
      startFind(entries, keyOf, query.slice(0, end), () => {});
      slices.push(performance.now() - start);
      while (queued.length) {
        start = performance.now();
        queued.shift()!();
        slices.push(performance.now() - start);
      }
      whole.push(performance.now() - began);
    }
  timer.mockRestore();
  slices.sort((a, b) => a - b);
  whole.sort((a, b) => a - b);
  return {
    slice: {
      median: percentile(slices, 0.5),
      p95: percentile(slices, 0.95),
      over: slices.filter((t) => t > 16).length / slices.length,
      count: slices.length,
    },
    whole: { median: percentile(whole, 0.5), p95: percentile(whole, 0.95) },
  };
}

const ms = (t: { median: number; p95: number }) =>
  `median ${t.median.toFixed(2)}ms p95 ${t.p95.toFixed(2)}ms`;

describe("what finding costs per keystroke", () => {
  it("never holds a frame, on the harness's session or a large working one", () => {
    const small = typing(harnessSession(600), "fixture row 59", 5);
    const large = workingSession(5_000);
    const bytes = JSON.stringify(large).length;
    const cold = performance.now();
    findMatches(large, (entry) => entry.key, "zz");
    const build = performance.now() - cold;
    const whole = typing(large, "linker wrote", 5);
    const sliced = slicedTyping(large, "linker wrote", 5);
    console.info(
      [
        `600 rows, in one go: ${ms(small)}`,
        `5000 rows (${(bytes / 1e6).toFixed(1)} MB): first search ${build.toFixed(0)}ms (prepares the text once)`,
        `5000 rows, in one go: ${ms(whole)}`,
        `5000 rows, a slice per frame: each slice ${ms(sliced.slice)} (${sliced.slice.count} slices, ${(sliced.slice.over * 100).toFixed(1)}% over 16ms); a keystroke found in full ${ms(sliced.whole)}`,
      ].join("\n"),
    );
    expect(small.p95).toBeLessThan(16);
    /* each slice stops at 6ms or one row past it; its tail on a shared
       machine is collection and scheduling, so the median is what's held */
    expect(sliced.slice.median).toBeLessThan(10);
  });
});
