export const NEEDS_TRIAGE = "needs-triage";
export const ROADMAP_PREFIX = "roadmap:";
export const HORIZONS = new Set([
  "roadmap: now",
  "roadmap: next",
  "roadmap: later",
  "roadmap: parked",
]);
// Exempt by issue NUMBER, not by label.
export const EXEMPT_ISSUES = new Set([839]);
export const ROADMAP_ISSUE = 839;

export const MARKER_PREFIX = "<!-- triage-hygiene:";
export const CONFLICT_MARKER_PREFIX = `${MARKER_PREFIX}conflict:`;
export const MISSING_HORIZON_MARKER_PREFIX = `${MARKER_PREFIX}missing-horizon`;
export const MISSING_HORIZON_MARKER = `${MISSING_HORIZON_MARKER_PREFIX} -->`;

// Enumerated explicitly, not inferred from MARKER_PREFIX: a future third
// marker must be added here deliberately rather than inheriting deletion.
export const RETRACTABLE_MARKER_PREFIXES = [
  CONFLICT_MARKER_PREFIX,
  MISSING_HORIZON_MARKER_PREFIX,
];

export const DEFAULT_BOT_LOGINS = ["github-actions[bot]"];

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

// A comment is deletable only if it carries a retractable marker prefix AND its
// author is one of this run's identities; never delete a comment it cannot
// prove it wrote.
function deletableBotNotices(comments, botLogins) {
  const logins = new Set(
    [...botLogins].filter(Boolean).map((l) => l.toLowerCase()),
  );
  return comments
    .map(normalizeComment)
    .filter((c) => c.id !== null && c.id !== undefined)
    .filter((c) => RETRACTABLE_MARKER_PREFIXES.some((p) => c.body.includes(p)))
    .filter(
      (c) => typeof c.author === "string" && logins.has(c.author.toLowerCase()),
    )
    .map((c) => c.id);
}

// run.mjs's gate on fetching comments; it must stay exactly the condition
// under which comments can change reconcile()'s answer. The property test in
// reconcile.test.mjs fences the equivalence.
export function requiresCommentContext({ number, labels, state = "open" }) {
  if (EXEMPT_ISSUES.has(number)) return false;
  if (state === "closed") return false;

  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX));
  if (roadmap.length === 0) return true;
  if (roadmap.length === 1 && HORIZONS.has(roadmap[0])) {
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

  if (state === "closed") {
    return flagged ? { ...noop, remove: [NEEDS_TRIAGE] } : noop;
  }

  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX)).sort();
  const unknown = roadmap.filter((l) => !HORIZONS.has(l));

  if (roadmap.length === 1 && unknown.length === 0) {
    // Retract both notices along with the flag; gated on `flagged`, the bot's
    // own record that it complained, so an unflagged clean issue costs no
    // comment reads.
    if (!flagged) return noop;
    return {
      add: [],
      remove: [NEEDS_TRIAGE],
      comment: null,
      deleteComments: deletableBotNotices(comments, botLogins),
    };
  }

  // Only mutate on a state change.
  const add = flagged ? [] : [NEEDS_TRIAGE];

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
