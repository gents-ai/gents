/* The transcript's activity, placed once.

   The folding before this (transcript-runs.ts, and tool-runs.ts inside a
   run) looked at the whole transcript on every render, so a call arriving
   later could change how earlier ones were shown: a lone row became part
   of an activity block, three reads became "looked through 3 files", a
   narration line became a note on a call. Each of those tore rows down and
   built new ones in a different place, which is what made things vanish,
   close, and move while a turn streamed.

   Here an item's place is decided by what came before it, never by what
   comes after. Consecutive tool calls share a group; anything that says
   something — the agent's narration or answer, the person's turn — ends
   it; a thought with no words in it stays inside the group it interrupts.
   A later call can only be added to the end of the last group or start a
   new one, so nothing already on screen is ever moved into a different
   container. What does change is a group's label, which is text in place. */
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
      /* the first tool group's key: fixed from the moment the group exists */
      key: string;
      members: GroupMember[];
      /* something after it has ended it; a settled group takes no more calls */
      settled: boolean;
    };

/* an item that says something ends a group; an empty or reasoning-only
   message is the agent thinking between calls, not talking */
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

/* The calls of one assistant message, in the order they were made. The
   desktop's transcript query orders them by tool_call_key as text, and a
   key ends in the call's position — "…:0", "…:1" … "…:14" — so as text
   "…:10" sorts before "…:2": in a burst of fifteen, calls 10–14 were shown
   between the second and the third, and each one arriving live was pushed
   into the middle of the group. A numeric-aware comparison puts them back
   in the order they ran, and a new call always lands at the end.

   This is a workaround for a runtime bug, not a rule of the transcript:
   the order belongs to the query (gents-desktop-core, session_transcript.rs,
   the `tool_call_key` order clauses), which should sort on the call's index
   as a number. Once it does, this sort is a no-op and can be removed. It
   does not fix the paged query, which selects rows in the same text order. */
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
    /* reasoning with nothing said, between calls: a step of the group */
    if (open && !says(item) && item.kind === "assistantMessage") {
      if (item.reasoning?.trim())
        open.members.push({ kind: "thought", key: item.itemKey, text: item.reasoning });
      continue;
    }
    /* the live tail with nothing in it yet is a status line, not speech:
       it neither ends the group nor joins it */
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

/* what a group did, in the words a person would use: "Read 6 files, ran 2
   commands, edited 5 files". Counts change as calls arrive; the words for
   a kind do not. */
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
            : p.kind === "subagent"
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
    n("worker") && `worked with ${plural(n("worker"), "agent", "agents")}`,
    n("other") && `used ${plural(n("other"), "tool", "tools")}`,
  ].filter(Boolean) as string[];
  const text = parts.join(", ") || "thought";
  return text[0]!.toUpperCase() + text.slice(1);
}

export const groupTools = (members: GroupMember[]) =>
  members.flatMap((m) => (m.kind === "tool" ? [m.tool] : []));

/* What a group is doing right now, while its newest call runs: "Editing
   SnakeGame.test.jsx…", "Running cargo test…". A group starts folded to
   its header, so this is how a live group shows its work without growing.
   Nothing is running — between calls, or once done — and the header says
   what the group did instead. */
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
          : p.kind === "subagent"
            ? `Working with ${p.name ?? "an agent"}`
            : `Using ${toolName ?? "a tool"}`;
  return `${doing}…`;
}
