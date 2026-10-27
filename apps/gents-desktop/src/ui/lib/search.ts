import type { ComponentType } from "react";
import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";
import type { Route } from "./router";

export type SearchKind = "attention" | "session" | "agent" | "setting" | "action";

type Icon = ComponentType<{ className?: string }>;

export type SearchTarget =
  | { kind: "attention"; item: MailboxItemView }
  | { kind: "session"; session: SessionSummary }
  | { kind: "agent"; route: Route; agentId: string; nodeDid: string }
  | { kind: "setting" | "action"; route: Route; icon: Icon };

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
  agents: readonly { agentId: string; nodeDid: string; name: string; node: string }[];
  agentName: (session: SessionSummary) => string;
  nodeName: (nodeDid: string) => string;
  places: readonly {
    key: string;
    title: string;
    group: string;
    route: Route;
    icon: Icon;
  }[];
  actions: readonly { key: string; title: string; route: Route; icon: Icon }[];
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
          agentId: a.agentId,
          nodeDid: a.nodeDid,
          route: {
            name: "agent",
            nodeDid: a.nodeDid,
            section: "agents",
            item: a.agentId,
          },
        },
        `agent:${a.nodeDid}:${a.agentId}`,
        a.name,
        a.node,
        [a.node],
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
      entry({ kind: "action", route: a.route, icon: a.icon }, a.key, a.title, "", []),
    ),
  ];
}

export type SearchFilter = "all" | Exclude<SearchKind, "attention">;
export type SearchGroup = {
  kind: SearchKind | "recent";
  label: string;
  entries: SearchEntry[];
};

const LABEL: Record<SearchKind, string> = {
  attention: "Needs attention",
  session: "Sessions",
  agent: "Agents",
  setting: "Settings",
  action: "Actions",
};
const ORDER: SearchKind[] = ["attention", "session", "agent", "setting", "action"];
const RECENT = 6;
const PER_GROUP = 5;
const FILTERED = 50;

const newest = (a: SearchEntry, b: SearchEntry) =>
  (b.at ?? "").localeCompare(a.at ?? "");

/** What the palette lists: with no query, what needs attention, recent
    sessions and actions; with one, the best matches of each kind, a few per
    kind under All and more under one kind. */
export function searchGroups(
  entries: readonly SearchEntry[],
  query: string,
  filter: SearchFilter,
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
        },
      ];
    const recent = of("session")
      .filter((e) => e.kind === "session" && !e.session.startedBy)
      .sort(newest)
      .slice(0, RECENT);
    return [
      {
        kind: "attention" as const,
        label: LABEL.attention,
        entries: of("attention").sort(newest),
      },
      { kind: "recent" as const, label: "Recent sessions", entries: recent },
      { kind: "action" as const, label: LABEL.action, entries: of("action") },
    ].filter((g) => g.entries.length > 0);
  }
  const cap = filter === "all" ? PER_GROUP : FILTERED;
  return ORDER.filter(shown)
    .map((kind) => ({
      kind,
      label: LABEL[kind],
      entries: entries
        .filter((e) => e.kind === kind)
        .map((e) => ({ e, score: entryScore(words, e) }))
        .filter((r) => r.score > 0)
        .sort((a, b) => b.score - a.score || newest(a.e, b.e))
        .slice(0, cap)
        .map((r) => r.e),
    }))
    .filter((g) => g.entries.length > 0);
}
