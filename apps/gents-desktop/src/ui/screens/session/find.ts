import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { createStore } from "zustand/vanilla";

import { readableReasoning, toolSummary } from "../tool-summary";
import type { TranscriptEntry } from "../transcript-groups";
import { withoutRoutingHeader } from "../TranscriptInputs";

/** Where a piece of a transcript's text is drawn: its row, and for an
    activity group the step or thought within it, and whether it is drawn
    only once that step or reasoning is opened. */
export type FindPlace = {
  rowKey: string;
  /** the reasoning to open: an assistant item's key, or a thought's */
  reasoning?: string;
  group?: { key: string; step?: string };
};

export type FindMatch = FindPlace & {
  /** which occurrence in its part of the row (see `findPart`), counted over
      that part's texts in order */
  nth: number;
};

/** The part of a row a match is counted in, as the page marks it with
    data-find-part: a step's output or a reasoning, which are drawn only
    once opened, or "" for the row's text that is always drawn. Counting in
    the part keeps a match's number to what the page draws, wherever other
    steps are folded. */
export const findPart = (place: FindPlace) =>
  place.group?.step
    ? `step:${place.group.step}`
    : place.reasoning
      ? `reasoning:${place.reasoning}`
      : "";

/* each source's text as it is found in, lowercased once: the items and
   steps keep their identity while a session updates, so typing searches
   text already prepared and a page that arrives prepares only its own */
const lowered = new WeakMap<object, readonly { hay: string; body: boolean }[]>();

const toolTexts = (tool: RenderedToolCallView) => {
  const kept = lowered.get(tool);
  if (kept) return kept;
  const summary = toolSummary(tool);
  const p = tool.presentation;
  const body = (() => {
    switch (p.kind) {
      case "command":
        return [p.stdout, p.stderr, p.fallbackOutput];
      case "fileRead":
        return [p.body, p.fallbackOutput];
      case "fileEdit":
        return [p.diff.map((line) => line.text).join("\n"), p.fallbackOutput];
      case "agent":
      case "process":
        return [p.description, p.output];
      case "mcp":
        return [p.arguments, p.output];
      case "generic":
        return [p.input, p.output];
      default:
        return [];
    }
  })();
  const texts = [
    {
      hay: [summary.kind, summary.full ?? summary.primary, summary.secondary]
        .filter(Boolean)
        .join(" ")
        .toLowerCase(),
      body: false,
    },
    ...body
      .filter((t): t is string => Boolean(t))
      .map((text) => ({ hay: text.toLowerCase(), body: true })),
  ];
  lowered.set(tool, texts);
  return texts;
};

const itemText = (item: RenderedTimelineItem): string | null => {
  switch (item.kind) {
    case "userMessage":
    case "assistantMessage":
    case "liveAssistant":
      return item.content;
    case "pendingUserTurn":
      return item.content;
    case "automatedInput":
      return item.origin.kind === "sessionMessage"
        ? (item.origin.displayBody ?? withoutRoutingHeader(item.content ?? ""))
        : item.content;
    case "sessionCompaction":
      return item.summary ?? null;
    default:
      return null;
  }
};

/* an item's reasoning, if it shows any (`body`), then its text */
const itemTexts = (item: RenderedTimelineItem) => {
  const kept = lowered.get(item);
  if (kept) return kept;
  const texts: { hay: string; body: boolean }[] = [];
  if (item.kind === "assistantMessage" || item.kind === "liveAssistant") {
    const reasoning = readableReasoning(item.reasoning);
    if (reasoning) texts.push({ hay: reasoning.toLowerCase(), body: true });
  }
  const text = itemText(item);
  if (text) texts.push({ hay: text.toLowerCase(), body: false });
  lowered.set(item, texts);
  return texts;
};

/** The texts a row draws, lowercased, each with what opens it, in
    drawing order. */
export function findTexts(
  entry: TranscriptEntry,
  rowKey: string,
): { hay: string; place: FindPlace }[] {
  const at: FindPlace = { rowKey };
  if (entry.kind === "group")
    return entry.members.flatMap((member) =>
      member.kind === "thought"
        ? [
            {
              hay: (readableReasoning(member.text) ?? "").toLowerCase(),
              place: { rowKey, reasoning: member.key, group: { key: entry.key } },
            },
          ]
        : toolTexts(member.tool).map(({ hay, body }) => ({
            hay,
            place: {
              rowKey,
              group: { key: entry.key, ...(body ? { step: member.key } : {}) },
            },
          })),
    );
  const { item } = entry;
  const out = itemTexts(item).map(({ hay, body }) => ({
    hay,
    place: body ? { rowKey, reasoning: item.itemKey } : at,
  }));
  for (const result of entry.background?.slice(1) ?? [])
    for (const { hay } of itemTexts(result.item)) out.push({ hay, place: at });
  return out;
}

/** How many matches are found before finding stops: a short query over a
    long session matches too often to count, and the reader steps through
    the ones nearest the newest. */
export const MATCH_LIMIT = 1000;

export type Found = { matches: FindMatch[]; more: boolean; done: boolean };

/* one row's matches, stopping once `room` are found */
function rowMatches(
  entry: TranscriptEntry,
  rowKey: string,
  needle: string,
  room: number,
) {
  const row: FindMatch[] = [];
  const counted = new Map<string, number>();
  for (const { hay, place } of findTexts(entry, rowKey)) {
    const part = findPart(place);
    for (
      let at = hay.indexOf(needle);
      at !== -1 && row.length < room;
      at = hay.indexOf(needle, at + needle.length)
    ) {
      const nth = counted.get(part) ?? 0;
      counted.set(part, nth + 1);
      row.push({ ...place, nth });
    }
  }
  return row;
}

/* newest rows first, a row a step, until `limit` and one more are found */
function scanner(
  entries: readonly TranscriptEntry[],
  keyOf: (entry: TranscriptEntry) => string,
  needle: string,
  limit: number,
) {
  const rows: FindMatch[][] = [];
  let count = 0;
  let i = entries.length - 1;
  return {
    /** searches the next row; false once there is nothing left to search */
    step() {
      if (i < 0 || count > limit) return false;
      const entry = entries[i--]!;
      const row = rowMatches(entry, keyOf(entry), needle, limit + 1 - count);
      if (row.length) rows.push(row);
      count += row.length;
      return true;
    },
    found(done: boolean): Found {
      const matches = rows.slice().reverse().flat();
      return count > limit
        ? { matches: matches.slice(matches.length - limit), more: true, done }
        : { matches, more: false, done };
    },
  };
}

/** The occurrences of `query`, ignoring case, in the rows' texts, in the
    order the rows are drawn: all of them, or the newest `limit` and more
    (`more`). */
export function findMatches(
  entries: readonly TranscriptEntry[],
  keyOf: (entry: TranscriptEntry) => string,
  query: string,
  limit = MATCH_LIMIT,
): { matches: FindMatch[]; more: boolean } {
  const needle = query.trim().toLowerCase();
  if (!needle) return { matches: [], more: false };
  const run = scanner(entries, keyOf, needle, limit);
  while (run.step());
  const { matches, more } = run.found(true);
  return { matches, more };
}

/** Finds as `findMatches` does, a slice of rows at a time so a long
    session never holds a frame: `onFound` hears the matches so far after
    each slice of about `budgetMs`, and last with `done`. Returns a stop. */
export function startFind(
  entries: readonly TranscriptEntry[],
  keyOf: (entry: TranscriptEntry) => string,
  query: string,
  onFound: (found: Found) => void,
  { limit = MATCH_LIMIT, budgetMs = 6 } = {},
): () => void {
  const needle = query.trim().toLowerCase();
  if (!needle) {
    onFound({ matches: [], more: false, done: true });
    return () => {};
  }
  const run = scanner(entries, keyOf, needle, limit);
  let timer = 0;
  const slice = () => {
    const until = performance.now() + budgetMs;
    while (run.step())
      if (performance.now() >= until) {
        onFound(run.found(false));
        timer = window.setTimeout(slice, 0);
        return;
      }
    onFound(run.found(true));
  };
  slice();
  return () => window.clearTimeout(timer);
}

/** Whether the open session's find bar shows; `focus` changes each time
    it is asked for, so asking again selects its text. */
export const findBar = createStore<{ open: boolean; focus: number }>(() => ({
  open: false,
  focus: 0,
}));

/** Opens the open session's find bar, or selects its text when it is open. */
export function openFind() {
  findBar.setState((s) => ({ open: true, focus: s.focus + 1 }));
}

export function closeFind() {
  findBar.setState({ open: false });
}
