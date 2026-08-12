import { test } from "node:test";
import assert from "node:assert/strict";
import { reconcile, conflictMarker, NEEDS_TRIAGE } from "./reconcile.mjs";

const base = { number: 1, labels: [], comments: [] };

test("exactly one horizon and no flag is already correct", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", "bug"] });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("exactly one horizon clears an existing flag", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", NEEDS_TRIAGE] });
  assert.deepEqual(r, { add: [], remove: [NEEDS_TRIAGE], comment: null });
});

test("every defined horizon is accepted", () => {
  for (const h of ["roadmap: now", "roadmap: next", "roadmap: later", "roadmap: parked"]) {
    assert.deepEqual(
      reconcile({ ...base, labels: [h] }),
      { add: [], remove: [], comment: null },
      `${h} should be valid`,
    );
  }
});

test("no horizon adds the flag without commenting", () => {
  const r = reconcile({ ...base, labels: ["bug"] });
  assert.deepEqual(r, { add: [NEEDS_TRIAGE], remove: [], comment: null });
});

test("no horizon with the flag already set writes nothing", () => {
  const r = reconcile({ ...base, labels: ["bug", NEEDS_TRIAGE] });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("two horizons flag and comment naming both labels", () => {
  const r = reconcile({ ...base, labels: ["roadmap: now", "roadmap: later"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.equal(r.remove.length, 0);
  assert.match(r.comment, /roadmap: later/);
  assert.match(r.comment, /roadmap: now/);
  assert.ok(r.comment.includes(conflictMarker(["roadmap: now", "roadmap: later"])));
});

test("an unknown roadmap label does not satisfy the rule", () => {
  const r = reconcile({ ...base, labels: ["roadmap: urgent"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.match(r.comment, /not a defined horizon/);
  assert.match(r.comment, /roadmap: urgent/);
});

test("one valid horizon plus one unknown is a conflict", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", "roadmap: urgent"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.match(r.comment, /roadmap: urgent/);
  assert.ok(r.comment.includes(conflictMarker(["roadmap: next", "roadmap: urgent"])));
});

test("an existing comment for the same conflict is not repeated", () => {
  const marker = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", "roadmap: later", NEEDS_TRIAGE],
    comments: [`stale text ${marker} more text`],
  });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("a different conflict set comments again", () => {
  const old = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", "roadmap: parked", NEEDS_TRIAGE],
    comments: [`older ${old}`],
  });
  assert.notEqual(r.comment, null);
});

test("the marker is independent of label order", () => {
  assert.equal(
    conflictMarker(["roadmap: now", "roadmap: later"]),
    conflictMarker(["roadmap: later", "roadmap: now"]),
  );
});

test("issue 839 is exempt by number even with no horizon", () => {
  const r = reconcile({ ...base, number: 839, labels: ["meta"] });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("a future meta issue is not exempt", () => {
  const r = reconcile({ ...base, number: 2000, labels: ["meta"] });
  assert.deepEqual(r, { add: [NEEDS_TRIAGE], remove: [], comment: null });
});
