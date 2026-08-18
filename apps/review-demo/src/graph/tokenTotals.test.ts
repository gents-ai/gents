import { describe, expect, it } from "vitest";

import { formatTokenTotals, tokenTotalsForRun } from "./tokenTotals.ts";

describe("tokenTotalsForRun", () => {
  it("sums only inference calls for the watched run", () => {
    const totals = tokenTotalsForRun(
      [
        { request_id: "a", prompt_tokens: 100, completion_tokens: 20 },
        { request_id: "b", prompt_tokens: 50, completion_tokens: 5 },
        { request_id: "c", prompt_tokens: 999, completion_tokens: 999 },
      ],
      [
        { request_id: "a", caused_by_correlation: "run-1" },
        { request_id: "b", caused_by_correlation: "run-1" },
        { request_id: "c", caused_by_correlation: "run-old" },
      ],
      "run-1",
    );
    expect(totals).toEqual({ prompt: 150, completion: 25, total: 175 });
    expect(formatTokenTotals(totals)).toBe("175 tokens · 150 in · 25 out");
  });

  it("returns zeros when no run is watched", () => {
    expect(tokenTotalsForRun([], [], null)).toEqual({
      prompt: 0,
      completion: 0,
      total: 0,
    });
  });
});
