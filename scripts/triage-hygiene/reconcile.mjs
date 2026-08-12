// Pure triage-hygiene decision logic. No I/O — see run.mjs for the API layer.
export const NEEDS_TRIAGE = "needs-triage";
export const ROADMAP_PREFIX = "roadmap:";
// The taxonomy defines exactly four horizons. Anything else under the
// `roadmap:` prefix is a typo, not a horizon, and must not satisfy the rule.
export const HORIZONS = new Set([
  "roadmap: now",
  "roadmap: next",
  "roadmap: later",
  "roadmap: parked",
]);
// Exempt by issue NUMBER, not by label: exempting the `meta` label would
// silently exempt every future meta issue.
export const EXEMPT_ISSUES = new Set([839]);
export const ROADMAP_ISSUE = 839;

// Every comment this bot writes opens with MARKER_PREFIX. The two families
// below extend it. Deletion matches on a *prefix* rather than a whole marker so
// a comment written for any prior conflict set is recognised.
export const MARKER_PREFIX = "<!-- triage-hygiene:";
export const CONFLICT_MARKER_PREFIX = `${MARKER_PREFIX}conflict:`;
// The missing-horizon notice is the same for every issue, so it needs no
// payload — one fixed marker dedupes it to at most one per issue.
export const MISSING_HORIZON_MARKER_PREFIX = `${MARKER_PREFIX}missing-horizon`;
export const MISSING_HORIZON_MARKER = `${MISSING_HORIZON_MARKER_PREFIX} -->`;

// Both notices are statements about a state the issue is no longer in, so both
// are retracted when it goes clean. Enumerated explicitly rather than matching
// MARKER_PREFIX: a future third marker must be added here deliberately, after
// someone has decided it is safe to delete, rather than inheriting deletion by
// accident of its name.
export const RETRACTABLE_MARKER_PREFIXES = [
  CONFLICT_MARKER_PREFIX,
  MISSING_HORIZON_MARKER_PREFIX,
];

// The identity a GitHub Actions run acts as when it uses the default
// GITHUB_TOKEN. run.mjs overrides this with the authenticated user when
// `GET /user` answers; it is the fallback, never a widening.
export const DEFAULT_BOT_LOGINS = ["github-actions[bot]"];

// Use JSON encoding to ensure the marker is unambiguous: label names may
// contain any character including separator characters, so encoding must be injective.
export function conflictMarker(labels) {
  return `${CONFLICT_MARKER_PREFIX}${JSON.stringify([...labels].sort())} -->`;
}

// Comments arrive as `{ id, body, author }`. A bare string is tolerated for
// dedup only: without an author it can never be a deletion candidate, which is
// the failure-safe direction — the bot must never delete a comment it cannot
// prove it wrote.
const normalizeComment = (c) =>
  typeof c === "string"
    ? { id: null, body: c, author: null }
    : { id: c?.id ?? null, body: c?.body ?? "", author: c?.author ?? null };

const hasMarker = (comments, marker) =>
  comments.map(normalizeComment).some((c) => c.body.includes(marker));

// The single guard on the one destructive capability in this bot. A comment is
// a deletion candidate only if BOTH hold: it carries one of this bot's own
// retractable marker prefixes, AND its author is one of the identities this run
// is acting as. A human comment quoting a marker fails the author test; a bot
// comment carrying no retractable marker fails the marker test.
function deletableBotNotices(comments, botLogins) {
  const logins = new Set([...botLogins].filter(Boolean).map((l) => l.toLowerCase()));
  return comments
    .map(normalizeComment)
    .filter((c) => c.id !== null && c.id !== undefined)
    .filter((c) => RETRACTABLE_MARKER_PREFIXES.some((p) => c.body.includes(p)))
    .filter((c) => typeof c.author === "string" && logins.has(c.author.toLowerCase()))
    .map((c) => c.id);
}

// Tells a caller whether reconcile() could possibly need comment context for
// this issue — either to dedupe a comment it is about to post, or to find its
// own stale notices to retract. Mirrors reconcile()'s branches
// exactly; kept here so run.mjs never re-derives the rule and risks drifting
// from it. The property test in reconcile.test.mjs fences the equivalence.
export function requiresCommentContext({ number, labels, state = "open" }) {
  if (EXEMPT_ISSUES.has(number)) return false;
  // A closed issue only ever has `needs-triage` removed; no comment is read,
  // written, or deleted.
  if (state === "closed") return false;

  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX));
  // Zero horizons posts the missing-horizon notice, which must be deduped.
  if (roadmap.length === 0) return true;
  if (roadmap.length === 1 && HORIZONS.has(roadmap[0])) {
    // Clean. The only reason to read comments is to retract a notice this bot
    // posted earlier, and `needs-triage` is the record that it posted one.
    return labels.includes(NEEDS_TRIAGE);
  }
  return true;
}

export function reconcile({
  number,
  labels,
  comments = [],
  state = "open",
  botLogins = DEFAULT_BOT_LOGINS,
}) {
  const noop = { add: [], remove: [], comment: null, deleteComments: [] };
  if (EXEMPT_ISSUES.has(number)) return noop;

  const flagged = labels.includes(NEEDS_TRIAGE);

  // A closed issue needs no horizon, so the flag is meaningless on it and the
  // sweep (state=open) can never reach it again. Clear it and stop — no
  // comment, no deletion, and never a horizon demand on closed work.
  if (state === "closed") {
    return flagged ? { ...noop, remove: [NEEDS_TRIAGE] } : noop;
  }

  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX)).sort();
  const unknown = roadmap.filter((l) => !HORIZONS.has(l));

  // Clean iff exactly one roadmap label AND it is a defined horizon.
  if (roadmap.length === 1 && unknown.length === 0) {
    // Both notices this bot writes describe a state the issue has now left:
    // the conflict comment accuses it of a horizon clash it no longer has, and
    // the missing-horizon notice says it carries none. Leaving either behind
    // leaves a false statement on a correctly-labelled issue forever — and
    // re-prioritizing through the label dropdown is two API calls, so the
    // transient states that trigger them are ordinary rather than mistakes.
    // Retract both along with the flag.
    //
    // Gated on `flagged`, which is the bot's own record that it complained:
    // an unflagged clean issue has nothing to retract, and reading its comments
    // would cost a request per issue on every sweep for nothing.
    if (!flagged) return noop;
    return {
      add: [],
      remove: [NEEDS_TRIAGE],
      comment: null,
      deleteComments: deletableBotNotices(comments, botLogins),
    };
  }

  // Only mutate on a state change; redundant writes create timeline noise and
  // can ping-pong via labeled/unlabeled events from non-GITHUB_TOKEN actors.
  const add = flagged ? [] : [NEEDS_TRIAGE];

  // No horizon yet is the most common path — every newly filed issue lands
  // here — so it gets a short, friendly explanation rather than a bare label.
  if (roadmap.length === 0) {
    if (hasMarker(comments, MISSING_HORIZON_MARKER)) {
      return { add, remove: [], comment: null, deleteComments: [] };
    }
    return {
      add,
      remove: [],
      comment:
        `Thanks for filing this. Every open issue carries exactly one \`roadmap:\` horizon so ` +
        `it lands somewhere on the plan — this one has none yet, so it is marked ` +
        `\`${NEEDS_TRIAGE}\`.\n\n` +
        `Add one of ${[...HORIZONS].map((l) => `\`${l}\``).join(", ")} and the flag clears ` +
        `automatically. See #${ROADMAP_ISSUE} for what the horizons mean.\n\n` +
        `${MISSING_HORIZON_MARKER}`,
      deleteComments: [],
    };
  }

  const marker = conflictMarker(roadmap);
  if (hasMarker(comments, marker)) {
    return { add, remove: [], comment: null, deleteComments: [] };
  }

  const detail =
    unknown.length > 0
      ? `${unknown.map((l) => `\`${l}\``).join(", ")} ${unknown.length === 1 ? "is not a" : "are not"} defined horizon${unknown.length === 1 ? "" : "s"}. Valid horizons: ${[...HORIZONS].map((l) => `\`${l}\``).join(", ")}.`
      : `It carries ${roadmap.length} horizons: ${roadmap.map((l) => `\`${l}\``).join(", ")}.`;

  return {
    add,
    remove: [],
    comment:
      `This issue does not carry exactly one valid \`roadmap:\` horizon. ${detail} ` +
      `See #${ROADMAP_ISSUE}. Fixing the labels clears \`${NEEDS_TRIAGE}\`.\n\n${marker}`,
    deleteComments: [],
  };
}
