import { test } from "node:test";
import assert from "node:assert/strict";
import {
  reconcile,
  conflictMarker,
  requiresCommentContext,
  MARKER_PREFIX,
  CONFLICT_MARKER_PREFIX,
  MISSING_HORIZON_MARKER,
  MISSING_HORIZON_MARKER_PREFIX,
  RETRACTABLE_MARKER_PREFIXES,
  DEFAULT_BOT_LOGINS,
  NEEDS_TRIAGE,
} from "./reconcile.mjs";

const BOT = DEFAULT_BOT_LOGINS[0];
const base = { number: 1, labels: [], comments: [], state: "open" };
const noop = { add: [], remove: [], comment: null, deleteComments: [] };
// Comments reach reconcile() as objects, because deletion has to know who
// wrote one. `id` must be present for a comment to be deletable at all.
const botComment = (id, body) => ({ id, body, author: BOT });
const humanComment = (id, body) => ({ id, body, author: "some-maintainer" });

test("exactly one horizon and no flag is already correct", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", "bug"] });
  assert.deepEqual(r, noop);
});

test("exactly one horizon clears an existing flag", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", NEEDS_TRIAGE] });
  assert.deepEqual(r, { add: [], remove: [NEEDS_TRIAGE], comment: null, deleteComments: [] });
});

test("every defined horizon is accepted", () => {
  for (const h of ["roadmap: now", "roadmap: next", "roadmap: later", "roadmap: parked"]) {
    assert.deepEqual(reconcile({ ...base, labels: [h] }), noop, `${h} should be valid`);
  }
});

test("no horizon adds the flag and explains how to clear it", () => {
  const r = reconcile({ ...base, labels: ["bug"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.deepEqual(r.remove, []);
  assert.deepEqual(r.deleteComments, []);
  assert.match(r.comment, /roadmap: now/);
  assert.match(r.comment, /roadmap: parked/);
  assert.match(r.comment, /#839/);
  assert.ok(r.comment.includes(MISSING_HORIZON_MARKER));
});

test("the missing-horizon notice is not accusatory", () => {
  const r = reconcile({ ...base, labels: ["bug"] });
  assert.doesNotMatch(r.comment, /does not carry|not a defined horizon/);
});

test("the missing-horizon notice is posted at most once per issue", () => {
  const r = reconcile({
    ...base,
    labels: ["bug", NEEDS_TRIAGE],
    comments: [botComment(1, `earlier ${MISSING_HORIZON_MARKER} trailing`)],
  });
  assert.deepEqual(r, noop);
});

test("no horizon with the flag set still explains itself the first time", () => {
  const r = reconcile({ ...base, labels: ["bug", NEEDS_TRIAGE] });
  assert.deepEqual(r.add, []);
  assert.ok(r.comment.includes(MISSING_HORIZON_MARKER));
});

test("a conflict comment does not dedupe the missing-horizon notice", () => {
  const r = reconcile({
    ...base,
    labels: ["bug", NEEDS_TRIAGE],
    comments: [botComment(1, `older ${conflictMarker(["roadmap: now", "roadmap: later"])}`)],
  });
  assert.ok(r.comment.includes(MISSING_HORIZON_MARKER));
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
    comments: [botComment(1, `stale text ${marker} more text`)],
  });
  assert.deepEqual(r, noop);
});

test("a different conflict set comments again", () => {
  const old = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", "roadmap: parked", NEEDS_TRIAGE],
    comments: [botComment(1, `older ${old}`)],
  });
  assert.notEqual(r.comment, null);
});

test("the marker is independent of label order", () => {
  assert.equal(
    conflictMarker(["roadmap: now", "roadmap: later"]),
    conflictMarker(["roadmap: later", "roadmap: now"]),
  );
});

test("the marker is injective across label sets that share a separator", () => {
  assert.notEqual(conflictMarker(["a|b"]), conflictMarker(["a", "b"]));
});

test("issue 839 is exempt by number even with no horizon", () => {
  const r = reconcile({ ...base, number: 839, labels: ["meta"] });
  assert.deepEqual(r, noop);
});

test("a future meta issue is not exempt", () => {
  const r = reconcile({ ...base, number: 2000, labels: ["meta"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
});

// ---------------------------------------------------------------------------
// H4: retracting the bot's own conflict comments once an issue is clean.
// Re-prioritizing through the label dropdown is two API calls, so a transient
// two-horizon state is ordinary. The accusatory comment must not outlive it.
// ---------------------------------------------------------------------------

test("a clean issue deletes the bot's stale conflict comment", () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: later", NEEDS_TRIAGE],
    comments: [botComment(4242, `text ${stale} tail`)],
  });
  assert.deepEqual(r.remove, [NEEDS_TRIAGE]);
  assert.deepEqual(r.deleteComments, [4242]);
  assert.equal(r.comment, null);
});

test("deletion matches the marker PREFIX, so any prior conflict set is found", () => {
  // Two different conflict sets, neither equal to the issue's current state.
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", NEEDS_TRIAGE],
    comments: [
      botComment(1, `a ${conflictMarker(["roadmap: now", "roadmap: later"])}`),
      botComment(2, `b ${conflictMarker(["roadmap: soon"])}`),
      botComment(3, `c ${CONFLICT_MARKER_PREFIX}["something else"] -->`),
    ],
  });
  assert.deepEqual(r.deleteComments, [1, 2, 3]);
});

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

test("a bot comment carrying no retractable marker is not deleted", () => {
  const r = reconcile({
    ...base,
    labels: ["roadmap: later", NEEDS_TRIAGE],
    comments: [
      botComment(7, "unrelated automation output"),
      botComment(8, "<!-- some-other-bot:note -->"),
      botComment(9, `mentions ${MARKER_PREFIX} but no known marker`),
    ],
  });
  assert.deepEqual(r.deleteComments, []);
});

test("a clean issue also retracts the bot's missing-horizon notice", () => {
  // It said "this one has none yet", which is now false. Same defect H4 fixed
  // for the conflict comment, and it fires on every newly filed issue that
  // later gets a horizon — the common path.
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", NEEDS_TRIAGE],
    comments: [botComment(31, `welcome ${MISSING_HORIZON_MARKER} tail`)],
  });
  assert.deepEqual(r.remove, [NEEDS_TRIAGE]);
  assert.deepEqual(r.deleteComments, [31]);
});

test("a clean issue retracts both notice types at once", () => {
  // The observed live sequence: filed with no horizon (explainer), then two
  // horizons added (conflict), then resolved. Both statements are now false.
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", NEEDS_TRIAGE],
    comments: [
      botComment(41, `welcome ${MISSING_HORIZON_MARKER}`),
      humanComment(42, "thanks, labelled it"),
      botComment(43, `conflict ${conflictMarker(["roadmap: now", "roadmap: later"])}`),
    ],
  });
  assert.deepEqual(r.deleteComments, [41, 43]);
});

test("a HUMAN comment quoting the missing-horizon marker is NEVER deleted", () => {
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", NEEDS_TRIAGE],
    comments: [humanComment(51, `why did I get ${MISSING_HORIZON_MARKER} on this?`)],
  });
  assert.deepEqual(r.remove, [NEEDS_TRIAGE]);
  assert.deepEqual(r.deleteComments, []);
});

test("a clean UNFLAGGED issue retracts nothing and needs no comment fetch", () => {
  const labels = ["roadmap: now"];
  assert.equal(requiresCommentContext({ number: 1, labels }), false);
  assert.deepEqual(
    reconcile({ ...base, labels, comments: [botComment(61, MISSING_HORIZON_MARKER)] }),
    noop,
  );
});

test("the retractable prefixes are enumerated, not inferred from MARKER_PREFIX", () => {
  assert.deepEqual(RETRACTABLE_MARKER_PREFIXES, [
    CONFLICT_MARKER_PREFIX,
    MISSING_HORIZON_MARKER_PREFIX,
  ]);
  // Every retractable prefix must be a strict extension of the family prefix,
  // and the family prefix itself must never be retractable on its own.
  for (const p of RETRACTABLE_MARKER_PREFIXES) {
    assert.ok(p.startsWith(MARKER_PREFIX) && p.length > MARKER_PREFIX.length);
  }
  assert.ok(!RETRACTABLE_MARKER_PREFIXES.includes(MARKER_PREFIX));
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

test("only the identities the run acts as are deletable", () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const comments = [botComment(1, stale), humanComment(2, stale)];
  const asBot = reconcile({ ...base, labels: ["roadmap: now", NEEDS_TRIAGE], comments });
  assert.deepEqual(asBot.deleteComments, [1]);

  // A run authenticated as the maintainer may retract its own comments.
  const asHuman = reconcile({
    ...base,
    labels: ["roadmap: now", NEEDS_TRIAGE],
    comments,
    botLogins: ["some-maintainer"],
  });
  assert.deepEqual(asHuman.deleteComments, [2]);
});

test("a clean issue with no comments deletes nothing and needs no fetch", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["roadmap: now"] }), false);
  assert.deepEqual(reconcile({ ...base, labels: ["roadmap: now"] }), noop);
  assert.deepEqual(reconcile({ ...base, labels: ["roadmap: now", NEEDS_TRIAGE] }), {
    add: [],
    remove: [NEEDS_TRIAGE],
    comment: null,
    deleteComments: [],
  });
});

test("only a flagged clean issue is worth reading comments for", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["roadmap: now"] }), false);
  assert.equal(
    requiresCommentContext({ number: 1, labels: ["roadmap: now", NEEDS_TRIAGE] }),
    true,
  );
});

test("an exempt issue never deletes comments", () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    number: 839,
    labels: ["roadmap: now", NEEDS_TRIAGE],
    comments: [botComment(1, stale)],
  });
  assert.deepEqual(r, noop);
});

// ---------------------------------------------------------------------------
// H6: a closed issue needs no horizon, and the sweep (state=open) can never
// reach it again, so the flag has to come off on the way out.
// ---------------------------------------------------------------------------

test("a closed issue has needs-triage removed and nothing else", () => {
  const r = reconcile({ ...base, labels: ["bug", NEEDS_TRIAGE], state: "closed" });
  assert.deepEqual(r, { add: [], remove: [NEEDS_TRIAGE], comment: null, deleteComments: [] });
});

test("a closed issue without the flag is left alone", () => {
  assert.deepEqual(reconcile({ ...base, labels: ["bug"], state: "closed" }), noop);
});

test("a closed issue is never flagged or commented on, whatever its horizons", () => {
  for (const labels of [[], ["roadmap: now", "roadmap: later"], ["roadmap: soon"], ["bug"]]) {
    const r = reconcile({ ...base, labels, state: "closed" });
    assert.deepEqual(r.add, [], `closed issue with ${JSON.stringify(labels)} must not be flagged`);
    assert.equal(r.comment, null);
  }
});

test("a closed issue never needs comment context", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: [], state: "closed" }), false);
  assert.equal(
    requiresCommentContext({ number: 1, labels: ["roadmap: now", NEEDS_TRIAGE], state: "closed" }),
    false,
  );
});

// ---------------------------------------------------------------------------
// requiresCommentContext / reconcile equivalence.
// ---------------------------------------------------------------------------

test("requiresCommentContext: exactly one valid horizon does not require comments", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["roadmap: next", "bug"] }), false);
});

test("requiresCommentContext: no roadmap labels requires comments (dedup the notice)", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["bug"] }), true);
});

test("requiresCommentContext: exactly one invalid roadmap label requires comments", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["roadmap: soon", "bug"] }), true);
});

test("requiresCommentContext: two valid horizons requires comments", () => {
  assert.equal(
    requiresCommentContext({ number: 1, labels: ["roadmap: now", "roadmap: later"] }),
    true,
  );
});

test("requiresCommentContext: exempt issue 839 with two horizons does not require comments", () => {
  assert.equal(
    requiresCommentContext({ number: 839, labels: ["roadmap: now", "roadmap: later"] }),
    false,
  );
});

// requiresCommentContext() is run.mjs's gate on fetching comments at all, so it
// must be *exactly* the condition under which comments can change reconcile()'s
// answer — in either direction now that reconcile() can also delete. If it says
// "no comments needed" for a case where comments matter, run.mjs calls
// reconcile() with an empty list and either reposts a notice forever or fails to
// retract a stale one. That exact pair drifted apart once already. Five
// hand-picked examples cannot fence an equivalence; enumerate the space instead.
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
            `changes once comments are read. run.mjs would act on the blind plan — ` +
            `reposting a notice forever, or leaving a stale accusation in place.`,
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
