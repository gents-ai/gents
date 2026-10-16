import { test } from "node:test";
import assert from "node:assert/strict";
import {
  reconcile,
  conflictMarker,
  requiresCommentContext,
  MISSING_HORIZON_MARKER,
  DEFAULT_BOT_LOGINS,
  NEEDS_TRIAGE,
} from "./reconcile.mjs";

const BOT = DEFAULT_BOT_LOGINS[0];
const base = { number: 1, labels: [], comments: [], state: "open" };
const botComment = (id, body) => ({ id, body, author: BOT });
const humanComment = (id, body) => ({ id, body, author: "some-maintainer" });

test("a human comment containing the marker is NEVER deleted", () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: later", NEEDS_TRIAGE],
    comments: [humanComment(99, `quoting the bot: ${stale} — why did this fire?`)],
  });
  assert.deepEqual(r.remove, [NEEDS_TRIAGE]);
  assert.deepEqual(r.deleteComments, []);
});

test("a comment with no author is never a deletion candidate", () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: later", NEEDS_TRIAGE],
    comments: [{ id: 5, body: stale, author: null }, stale],
  });
  assert.deepEqual(r.deleteComments, []);
});

// requiresCommentContext() is run.mjs's gate on fetching comments at all, so it
// must be exactly the condition under which comments can change reconcile()'s
// answer — the pair drifted apart once already. Five hand-picked examples
// cannot fence an equivalence; enumerate the space instead.
const ALPHABET = [
  "roadmap: now",
  "roadmap: next",
  "roadmap: later",
  "roadmap: parked",
  "roadmap: soon", // unknown horizon: `roadmap:`-prefixed but not defined
  "bug", // not roadmap-prefixed at all
  NEEDS_TRIAGE,
];

const labelSetsUpToSize = (alphabet, maxSize) => {
  const sets = [[]];
  const extend = (prefix, start) => {
    if (prefix.length === maxSize) return;
    for (let i = start; i < alphabet.length; i += 1) {
      const next = [...prefix, alphabet[i]];
      sets.push(next);
      extend(next, i + 1);
    }
  };
  extend([], 0);
  return sets;
};

// A comment list that would change the answer if it were ever read: it carries
// both marker families and is authored by the bot, so it is simultaneously a
// dedup hit and a deletion candidate.
const LOADED_COMMENTS = [
  botComment(101, `stale ${conflictMarker(["roadmap: now", "roadmap: later"])}`),
  botComment(102, `welcome ${MISSING_HORIZON_MARKER}`),
];

test("property: not requiring comment context implies comments cannot change the plan", () => {
  const sets = labelSetsUpToSize(ALPHABET, 3);
  assert.equal(sets.length, 64, "expected every subset of size 0..3");
  let checked = 0;
  for (const number of [1, 839]) {
    for (const state of ["open", "closed"]) {
      for (const labels of sets) {
        if (requiresCommentContext({ number, labels, state })) continue;
        const blind = reconcile({ number, labels, state, comments: [] });
        const informed = reconcile({ number, labels, state, comments: LOADED_COMMENTS });
        assert.deepEqual(
          blind,
          informed,
          `#${number} (${state}) with labels ${JSON.stringify(labels)}: ` +
            `requiresCommentContext() said no comment context was needed, but the plan ` +
            `changes once comments are read.`,
        );
        assert.equal(blind.comment, null, "a blind plan must never post a comment");
        assert.deepEqual(blind.deleteComments, [], "a blind plan must never delete a comment");
        checked += 1;
      }
    }
  }
  assert.ok(checked > 0, "the property must actually exercise some label sets");
});

test("property: a human's comment is never deleted, whatever the labels", () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const comments = [humanComment(1, stale), humanComment(2, MISSING_HORIZON_MARKER)];
  let checked = 0;
  for (const number of [1, 839]) {
    for (const state of ["open", "closed"]) {
      for (const labels of labelSetsUpToSize(ALPHABET, 3)) {
        const r = reconcile({ number, labels, state, comments });
        assert.deepEqual(
          r.deleteComments,
          [],
          `#${number} (${state}) with labels ${JSON.stringify(labels)} would delete a human's comment`,
        );
        checked += 1;
      }
    }
  }
  assert.ok(checked > 0);
});

test("regression: single invalid roadmap label dedupes against a prior conflict comment", () => {
  const marker = conflictMarker(["roadmap: soon"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: soon", NEEDS_TRIAGE],
    comments: [botComment(1, `stale text ${marker} more text`)],
  });
  assert.equal(r.comment, null);
});
