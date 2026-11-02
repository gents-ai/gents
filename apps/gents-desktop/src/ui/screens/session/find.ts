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
  /** which occurrence in its row, counted over the row's texts in order */
  nth: number;
};

const toolTexts = (tool: RenderedToolCallView): { text: string; body: boolean }[] => {
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
  return [
    {
      text: [summary.kind, summary.full ?? summary.primary, summary.secondary]
        .filter(Boolean)
        .join(" "),
      body: false,
    },
    ...body
      .filter((t): t is string => Boolean(t))
      .map((text) => ({ text, body: true })),
  ];
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

/** The texts a row draws, each with what opens it, in drawing order. */
export function findTexts(
  entry: TranscriptEntry,
  rowKey: string,
): { text: string; place: FindPlace }[] {
  const at: FindPlace = { rowKey };
  if (entry.kind === "group")
    return entry.members.flatMap((member) =>
      member.kind === "thought"
        ? [
            {
              text: readableReasoning(member.text) ?? "",
              place: { rowKey, reasoning: member.key, group: { key: entry.key } },
            },
          ]
        : toolTexts(member.tool).map(({ text, body }) => ({
            text,
            place: {
              rowKey,
              group: { key: entry.key, ...(body ? { step: member.key } : {}) },
            },
          })),
    );
  const out: { text: string; place: FindPlace }[] = [];
  const { item } = entry;
  if (item.kind === "assistantMessage" || item.kind === "liveAssistant") {
    const reasoning = readableReasoning(item.reasoning);
    if (reasoning)
      out.push({ text: reasoning, place: { rowKey, reasoning: item.itemKey } });
  }
  const text = itemText(item);
  if (text) out.push({ text, place: at });
  for (const result of entry.background?.slice(1) ?? [])
    if (result.item.content) out.push({ text: result.item.content, place: at });
  return out;
}

/** Every occurrence of `query`, ignoring case, in the rows' texts, in the
    order the rows are drawn. */
export function findMatches(
  entries: readonly TranscriptEntry[],
  keyOf: (entry: TranscriptEntry) => string,
  query: string,
): FindMatch[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];
  const out: FindMatch[] = [];
  for (const entry of entries) {
    const rowKey = keyOf(entry);
    let nth = 0;
    for (const { text, place } of findTexts(entry, rowKey)) {
      const hay = text.toLowerCase();
      for (
        let at = hay.indexOf(needle);
        at !== -1;
        at = hay.indexOf(needle, at + needle.length)
      )
        out.push({ ...place, nth: nth++ });
    }
  }
  return out;
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
