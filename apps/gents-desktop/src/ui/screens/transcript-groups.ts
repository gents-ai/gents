/* The transcript's activity, placed once: an item's place is decided by
   what came before it, never by what comes after. */
import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";

export type GroupMember =
  | { kind: "tool"; key: string; tool: RenderedToolCallView }
  | { kind: "thought"; key: string; text: string };

export type TranscriptEntry =
  | { kind: "item"; key: string; item: RenderedTimelineItem }
  | {
      kind: "group";
      key: string;
      members: GroupMember[];
      /* something after it has ended it; a settled group takes no more calls */
      settled: boolean;
    };

function says(item: RenderedTimelineItem): boolean {
  switch (item.kind) {
    case "toolGroup":
      return false;
    case "assistantMessage":
    case "liveAssistant":
      return Boolean(item.content?.trim());
    default:
      return true;
  }
}

/* The calls of one assistant message, in the order they were made: the
   transcript query sorts tool_call_key as text, so "…:10" sorts before
   "…:2". Remove once the query sorts numerically. */
const inCallOrder = (tools: RenderedToolCallView[]) =>
  [...tools].sort((a, b) =>
    a.itemKey.localeCompare(b.itemKey, undefined, { numeric: true }),
  );

export function groupTranscript(items: RenderedTimelineItem[]): TranscriptEntry[] {
  const out: TranscriptEntry[] = [];
  let open: Extract<TranscriptEntry, { kind: "group" }> | null = null;
  for (const item of items) {
    if (item.kind === "toolGroup") {
      if (item.tools.length === 0) continue;
      if (!open) {
        open = { kind: "group", key: item.itemKey, members: [], settled: false };
        out.push(open);
      }
      for (const tool of inCallOrder(item.tools))
        open.members.push({ kind: "tool", key: tool.itemKey, tool });
      continue;
    }
    /* reasoning with nothing said: a step of the group */
    if (open && !says(item) && item.kind === "assistantMessage") {
      if (item.reasoning?.trim())
        open.members.push({ kind: "thought", key: item.itemKey, text: item.reasoning });
      continue;
    }
    /* the live tail with nothing in it yet is a status line, not speech */
    if (item.kind === "liveAssistant" && !says(item)) {
      out.push({ kind: "item", key: item.itemKey, item });
      continue;
    }
    if (open) open.settled = true;
    open = null;
    out.push({ kind: "item", key: item.itemKey, item });
  }
  return out;
}

export function groupLabel(members: GroupMember[]): string {
  const counts = new Map<string, number>();
  for (const m of members) {
    if (m.kind !== "tool") continue;
    const p = m.tool.presentation;
    const kind =
      p.kind === "fileRead"
        ? "read"
        : p.kind === "fileEdit"
          ? "edit"
          : p.kind === "command"
            ? "command"
            : p.kind === "agent"
              ? "worker"
              : "other";
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  const n = (k: string) => counts.get(k) ?? 0;
  const plural = (count: number, one: string, many: string) =>
    `${count} ${count === 1 ? one : many}`;
  const parts = [
    n("read") && `read ${plural(n("read"), "file", "files")}`,
    n("edit") && `edited ${plural(n("edit"), "file", "files")}`,
    n("command") && `ran ${plural(n("command"), "command", "commands")}`,
    n("worker") && `made ${plural(n("worker"), "session call", "session calls")}`,
    n("other") && `used ${plural(n("other"), "tool", "tools")}`,
  ].filter(Boolean) as string[];
  const text = parts.join(", ") || "thought";
  return text[0]!.toUpperCase() + text.slice(1);
}

export const groupTools = (members: GroupMember[]) =>
  members.flatMap((m) => (m.kind === "tool" ? [m.tool] : []));

export function liveGroupLabel(members: GroupMember[]): string | null {
  const last = members[members.length - 1];
  if (!last || last.kind !== "tool") return null;
  const { statusKind, presentation: p, toolName } = last.tool;
  if (statusKind !== "running") return null;
  const file = (path: string | null) =>
    (path ?? "").split(/[\\/]/).filter(Boolean).pop() ?? "a file";
  const line = (text: string) => (text.length > 60 ? `${text.slice(0, 59)}…` : text);
  const doing =
    p.kind === "fileRead"
      ? `Reading ${file(p.target)}`
      : p.kind === "fileEdit"
        ? `${p.created ? "Writing" : "Editing"} ${file(p.path)}`
        : p.kind === "command"
          ? `Running ${line(p.command)}`
          : p.kind === "agent"
            ? `Working with ${p.name ?? "an agent"}`
            : `Using ${toolName ?? "a tool"}`;
  return `${doing}…`;
}
