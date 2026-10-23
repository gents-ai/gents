import { describe, expect, it } from "vitest";
import { createHeightLearner, guessHeight, linesOf } from "../src/ui/lib/rowHeights";

describe("a row's height before it is drawn", () => {
  it("wraps each paragraph in the column and keeps a fenced block's lines", () => {
    const long = "word ".repeat(60);
    expect(linesOf(long, 760).prose).toBeLessThan(linesOf(long, 380).prose);
    expect(linesOf("one\n\ntwo", 760).prose).toBe(2);
    expect(linesOf("```\na\nb\n```", 760)).toEqual({ prose: 1, code: 4 });
  });

  it("grows with what the row says and holds a folded group to its header", () => {
    const short = guessHeight({ kind: "reply", text: "ok" }, 760);
    const long = guessHeight({ kind: "reply", text: "word ".repeat(400) }, 760);
    expect(long).toBeGreaterThan(short * 5);
    expect(guessHeight({ kind: "group", open: false, steps: 30 }, 760)).toBe(48);
    expect(guessHeight({ kind: "group", open: true, steps: 3 }, 760)).toBeGreaterThan(
      48,
    );
  });

  it("learns each kind's error from the first rows drawn, then holds it", () => {
    const learner = createHeightLearner();
    expect(learner.scale("reply")).toBe(1);
    for (let n = 0; n < 24; n += 1) learner.learn("reply", 100, 80);
    expect(learner.scale("reply")).toBeCloseTo(0.8);
    for (let n = 0; n < 24; n += 1) learner.learn("reply", 100, 200);
    expect(learner.scale("reply")).toBeCloseTo(0.8);
    expect(learner.scale("user")).toBe(1);
  });
});
