import type { ComponentType } from "react";
import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";
import type { Route } from "./router";

export type SearchKind =
  "attention" | "node" | "session" | "agent" | "automation" | "setting" | "action";

type Icon = ComponentType<{ className?: string }>;

export type SearchTarget =
  | { kind: "attention"; item: MailboxItemView }
  | {
      kind: "node";
      nodeDid: string;
      name: string;
      avatar: string | null;
      /** the node the app works with now */
      current?: boolean;
      /** the one before it, which the last-node key goes back to */
      last?: boolean;
    }
  | { kind: "session"; session: SessionSummary }
  | { kind: "agent"; route: Route; agentDid: string; nodeDid: string }
  | { kind: "automation"; route: Route; icon: Icon }
  | { kind: "setting"; route: Route; icon: Icon }
  | { kind: "action"; run: () => void; icon: Icon; keys?: string };

export type SearchEntry = SearchTarget & {
  key: string;
  title: string;
  subtitle: string;
  /* lowercased once, when the stores change, not per keystroke */
  hay: { title: string; rest: string[] };
  at: string | null;
};

const entry = (
  target: SearchTarget,
  key: string,
  title: string,
  subtitle: string,
  rest: string[],
  at: string | null = null,
): SearchEntry => ({
  ...target,
  key,
  title,
  subtitle,
  hay: { title: title.toLowerCase(), rest: rest.map((t) => t.toLowerCase()) },
  at,
});

export const queryWords = (query: string) =>
  query.toLowerCase().split(/\s+/).filter(Boolean);

/** How well lowercased `text` matches the query's words: 0 when some word
    is not found, otherwise higher for a whole-text prefix, then a word
    start, then anywhere inside. */
export function matchScore(words: readonly string[], text: string): number {
  if (words.length === 0) return 0;
  let score = 0;
  for (const word of words) {
    const at = text.indexOf(word);
    if (at < 0) return 0;
    if (at === 0) score += 3;
    else if (startsWord(text, word)) score += 2;
    else score += 1;
  }
  return score;
}

function startsWord(text: string, word: string) {
  for (let at = text.indexOf(word); at >= 0; at = text.indexOf(word, at + 1)) {
    if (!/[\p{L}\p{N}]/u.test(text[at - 1]!)) return true;
  }
  return false;
}

/** A title match ranks above a match in the subtitle or other texts. */
export function entryScore(words: readonly string[], e: SearchEntry) {
  const own = matchScore(words, e.hay.title) * 2;
  return Math.max(own, ...e.hay.rest.map((text) => matchScore(words, text)));
}

export type SearchSources = {
  sessions: readonly SessionSummary[];
  attention: readonly MailboxItemView[];
  /** every agent, with its node and the pack it came from, if any */
  agents: readonly {
    agentDid: string;
    nodeDid: string;
    name: string;
    node: string;
    pack: string | null;
  }[];
  /** every node, with what it is to this app and whether it answers, in the
      order a Nodes view lists them before anything is typed */
  nodes: readonly {
    nodeDid: string;
    name: string;
    /** the node's saved portrait, if it has one */
    avatar: string | null;
    place: string;
    offline: boolean;
    current?: boolean;
    last?: boolean;
  }[];
  /** the active node's automations, while the Automations page is on */
  automations: readonly { id: string; name: string; when: string; icon: Icon }[];
  agentName: (session: SessionSummary) => string;
  nodeName: (nodeDid: string) => string;
  places: readonly {
    key: string;
    title: string;
    group: string;
    route: Route;
    icon: Icon;
  }[];
  actions: readonly {
    key: string;
    title: string;
    run: () => void;
    icon: Icon;
    keys?: string;
  }[];
};

export function searchEntries(src: SearchSources): SearchEntry[] {
  return [
    ...src.attention.map((item) =>
      entry(
        { kind: "attention", item },
        `attention:${item.itemId}`,
        item.title,
        item.summary ?? "",
        [item.summary ?? ""],
        item.createdAt,
      ),
    ),
    ...src.nodes.map((n) =>
      entry(
        {
          kind: "node",
          nodeDid: n.nodeDid,
          name: n.name,
          avatar: n.avatar,
          current: n.current,
          last: n.last,
        },
        `node:${n.nodeDid}`,
        n.name,
        [n.place, n.offline && "Offline"].filter(Boolean).join(" · "),
        [n.place],
      ),
    ),
    ...src.sessions.map((session) => {
      const agent = src.agentName(session);
      const node = src.nodeName(session.nodeDid);
      return entry(
        { kind: "session", session },
        `session:${session.nodeDid}:${session.sessionId}`,
        session.title ?? "Untitled",
        `${agent} · ${node}`,
        [agent, node, session.sessionId],
        session.updatedAt,
      );
    }),
    ...src.agents.map((a) =>
      entry(
        {
          kind: "agent",
          agentDid: a.agentDid,
          nodeDid: a.nodeDid,
          route: {
            name: "agent",
            nodeDid: a.nodeDid,
            section: "agents",
            item: a.agentDid,
          },
        },
        `agent:${a.nodeDid}:${a.agentDid}`,
        a.name,
        a.pack ? `${a.node} · ${a.pack}` : a.node,
        a.pack ? [a.node, a.pack] : [a.node],
      ),
    ),
    ...src.automations.map((a) =>
      entry(
        {
          kind: "automation",
          route: { name: "automations", item: a.id },
          icon: a.icon,
        },
        `automation:${a.id}`,
        a.name,
        a.when,
        [a.when],
      ),
    ),
    ...src.places.map((p) =>
      entry(
        { kind: "setting", route: p.route, icon: p.icon },
        p.key,
        p.title,
        p.group,
        [p.group],
      ),
    ),
    ...src.actions.map((a) =>
      entry(
        { kind: "action", run: a.run, icon: a.icon, keys: a.keys },
        a.key,
        a.title,
        "",
        [],
      ),
    ),
  ];
}

export type SearchFilter = "all" | Exclude<SearchKind, "attention">;
export type SearchGroup = {
  kind: SearchKind | "recent";
  label: string;
  entries: SearchEntry[];
  /** how many more there are past the ones listed, until the group is expanded */
  more: number;
};

const LABEL: Record<SearchKind, string> = {
  attention: "Needs attention",
  node: "Nodes",
  session: "Sessions",
  agent: "Agents",
  automation: "Automations",
  setting: "Settings",
  action: "Actions",
};
const ORDER: SearchKind[] = [
  "attention",
  "node",
  "session",
  "agent",
  "automation",
  "setting",
  "action",
];
/* before anything is typed, a few of each, so the palette opens short */
const OPENING: Record<"attention" | "recent" | "action", number> = {
  attention: 3,
  recent: 5,
  action: 3,
};
const PER_GROUP = 5;
const FILTERED = 50;

/* the first `cap`, and how many are left past them; an expanded group
   lists up to the cap one kind gets on its own */
function capped(
  kind: SearchGroup["kind"],
  label: string,
  all: SearchEntry[],
  cap: number,
  expanded: ReadonlySet<string>,
): SearchGroup {
  const limit = expanded.has(kind) ? FILTERED : cap;
  return {
    kind,
    label,
    entries: all.slice(0, limit),
    more: Math.max(0, Math.min(all.length, FILTERED) - limit),
  };
}

const newest = (a: SearchEntry, b: SearchEntry) =>
  (b.at ?? "").localeCompare(a.at ?? "");

/** What the palette lists: with no query, what needs attention, recent
    sessions and actions, a few of each; with one, the best matches of each
    kind, a few per kind under All and more under one kind. A group listing
    fewer than it has says how many more, and lists them once `expanded`
    names its kind. */
export function searchGroups(
  entries: readonly SearchEntry[],
  query: string,
  filter: SearchFilter,
  expanded: ReadonlySet<string> = new Set(),
): SearchGroup[] {
  const words = queryWords(query);
  const shown = (kind: SearchKind) => filter === "all" || filter === kind;
  if (words.length === 0) {
    const of = (kind: SearchKind) => entries.filter((e) => e.kind === kind);
    if (filter !== "all")
      return [
        {
          kind: filter,
          label: LABEL[filter],
          entries: of(filter).sort(newest).slice(0, FILTERED),
          more: 0,
        },
      ];
    const recent = of("session")
      .filter((e) => e.kind === "session" && !e.session.startedBy)
      .sort(newest);
    return [
      capped(
        "attention",
        LABEL.attention,
        of("attention").sort(newest),
        OPENING.attention,
        expanded,
      ),
      capped("recent", "Recent sessions", recent, OPENING.recent, expanded),
      capped("action", LABEL.action, of("action"), OPENING.action, expanded),
    ].filter((g) => g.entries.length > 0);
  }
  const cap = filter === "all" ? PER_GROUP : FILTERED;
  return ORDER.filter(shown)
    .map((kind) =>
      capped(
        kind,
        LABEL[kind],
        entries
          .filter((e) => e.kind === kind)
          .map((e) => ({ e, score: entryScore(words, e) }))
          .filter((r) => r.score > 0)
          .sort((a, b) => b.score - a.score || newest(a.e, b.e))
          .map((r) => r.e),
        cap,
        expanded,
      ),
    )
    .filter((g) => g.entries.length > 0);
}
