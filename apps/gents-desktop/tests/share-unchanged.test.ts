import { describe, expect, it } from "vitest";

import { shareUnchanged } from "../src/hooks/fleetStore";

describe("sharing what a read did not change", () => {
  it("keeps the previous object for every unchanged part, and a new one where it changed", () => {
    const prev = {
      syncHealth: { state: "healthy" },
      nodes: [{ id: "a" }, { id: "b" }],
      n: 1,
    };
    const next = {
      syncHealth: { state: "healthy" },
      nodes: [{ id: "a" }, { id: "b2" }],
      n: 1,
    };
    const shared = shareUnchanged(prev, next);
    expect(shared).toEqual(next);
    expect(shared).not.toBe(prev);
    expect(shared.syncHealth).toBe(prev.syncHealth);
    expect(shared.nodes[0]).toBe(prev.nodes[0]);
    expect(shared.nodes[1]).toEqual(next.nodes[1]);
    expect(shared.nodes[1]).not.toBe(prev.nodes[1]);
  });

  it("returns the previous value whole when nothing changed", () => {
    const prev = { a: [1, { b: null }], c: "x" };
    expect(shareUnchanged(prev, { a: [1, { b: null }], c: "x" })).toBe(prev);
  });

  it("takes the new value where the shape changed", () => {
    const next = { a: [1] };
    expect(shareUnchanged({ a: { 0: 1 } }, next).a).toBe(next.a);
    expect(shareUnchanged(null, next)).toBe(next);
  });
});
