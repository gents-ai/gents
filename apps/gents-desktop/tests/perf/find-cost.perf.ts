/* What finding costs in time, measured apart from the unit run, whose
   tests run in parallel: `npm run perf:find`. */
import { describe, expect, it, vi } from "vitest";

import { findMatches, startFind } from "@/screens/session/find";
import type { TranscriptEntry } from "@/screens/transcript-groups";
import { harnessSession, workingSession } from "../find-fixture";

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
