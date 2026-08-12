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

const MARKER_OPEN = "<!-- triage-hygiene:conflict:";

// Use JSON encoding to ensure the marker is unambiguous: label names may
// contain any character including separator characters, so encoding must be injective.
export function conflictMarker(labels) {
  return `${MARKER_OPEN}${JSON.stringify([...labels].sort())} -->`;
}

export function reconcile({ number, labels, comments = [] }) {
  const noop = { add: [], remove: [], comment: null };
  if (EXEMPT_ISSUES.has(number)) return noop;

  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX)).sort();
  const unknown = roadmap.filter((l) => !HORIZONS.has(l));
  const flagged = labels.includes(NEEDS_TRIAGE);

  // Clean iff exactly one roadmap label AND it is a defined horizon.
  if (roadmap.length === 1 && unknown.length === 0) {
    return flagged ? { add: [], remove: [NEEDS_TRIAGE], comment: null } : noop;
  }

  // Only mutate on a state change; redundant writes create timeline noise and
  // can ping-pong via labeled/unlabeled events from non-GITHUB_TOKEN actors.
  const add = flagged ? [] : [NEEDS_TRIAGE];

  // Nothing to explain when the issue simply has no horizon yet.
  if (roadmap.length === 0) return { add, remove: [], comment: null };

  const marker = conflictMarker(roadmap);
  if (comments.some((body) => body.includes(marker))) {
    return { add, remove: [], comment: null };
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
      `See #839. Fixing the labels clears \`${NEEDS_TRIAGE}\`.\n\n${marker}`,
  };
}
