/* One line per tool call, in words a person would use: the desktop's
   ToolGroup summary, as data so the transcript steps and the trace can
   both label a call the same way. */
import type {
  RenderedToolCallView,
  ToolDiffLineKind,
  ToolDiffLineView,
  ToolPresentationView,
} from "@source-inc/gents-desktop-client";
import { shortPath } from "./tool-runs";
import type { ToolStepStatus } from "@gents/ui/conversation";

/* `cd <somewhere> && real-command …` is how a shell tool is usually
   called, and the cd is scaffolding: a real export had it leading 1,037 of
   1,525 commands. The row says what ran; the body still has the whole of
   it, exactly as it was issued. */
export { shortPath };

/* leading VAR=value assignments before a command; the command is what reads */
const ASSIGNMENTS = /^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S*)(?:\s+|$))+/;

export const spoken = (command: string): string => {
  const parts = command
    .split("&&")
    .map((part) => part.trim().replace(ASSIGNMENTS, "").trim())
    .filter((part) => part && !/^cd\b/.test(part));
  return parts.length ? parts.join(" && ") : command.trim();
};

export const DIFF_MARK: Record<ToolDiffLineKind, string> = {
  added: "+",
  removed: "-",
  context: " ",
};

export const diffText = (diff: ToolDiffLineView[]) =>
  diff.map((line) => `${DIFF_MARK[line.kind]}${line.text}`).join("\n");

/* a final newline ends the last line; it does not start another */
export const lineCount = (text: string) =>
  text ? text.replace(/\r?\n$/, "").split("\n").length : 0;

export const isAbsolutePath = (path: string) => /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(path);

const compact = (value: string | null | undefined, max = 80) => {
  const flat = value?.replace(/\s+/g, " ").trim();
  if (!flat) return null;
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
};
export const duration = (ms: number) => {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1).replace(/\.0$/, "")}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return s ? `${m}m ${s}s` : `${m}m`;
};
const exit = (p: Extract<ToolPresentationView, { kind: "command" }>) =>
  p.timedOut
    ? "timed out"
    : p.exitCode != null
      ? `exit ${p.exitCode}`
      : p.failed
        ? "failed"
        : null;

const readCount = (p: Extract<ToolPresentationView, { kind: "fileRead" }>) => {
  if (p.returnedCount == null) return null;
  const total =
    p.totalCount != null && p.totalCount !== p.returnedCount
      ? ` of ${p.totalCount}`
      : "";
  return `${p.returnedCount}${total}${p.truncated ? " · truncated" : ""}`;
};

/* one line: what the tool did, in words a person would use */
/* a policy denial reads as what was refused, not as a failed tool */
export function toolSummary(t: RenderedToolCallView): {
  kind: string;
  primary: string;
  secondary?: string | null;
  mono?: boolean;
} {
  const p = t.presentation;
  if (t.denial && t.statusKind === "error")
    return {
      kind: `denied · ${t.denial.categoryLabel}`,
      primary: t.denial.deniedCommand ?? t.toolName,
      secondary: t.denial.reasonLine,
      mono: Boolean(t.denial.deniedCommand),
    };
  switch (p.kind) {
    case "command":
      return { kind: "$", primary: spoken(p.command), secondary: exit(p), mono: true };
    case "fileRead":
      return {
        kind: p.operation.replace("_file", ""),
        primary: p.target ? shortPath(p.target) : t.toolName,
        secondary: readCount(p),
        mono: true,
      };
    case "fileEdit": {
      const verb =
        p.created === true
          ? "created"
          : p.created === false && p.operation === "write_file"
            ? "overwrote"
            : t.statusKind === "running"
              ? p.operation === "write_file"
                ? "writing"
                : "editing"
              : "edited";
      return {
        kind: verb,
        primary: p.path ? shortPath(p.path) : t.toolName,
        secondary:
          p.replacementsApplied != null && p.replacementsApplied > 1
            ? `×${p.replacementsApplied}`
            : null,
        mono: true,
      };
    }
    case "subagent":
      return {
        kind: `subagent · ${p.action}`,
        primary: p.action === "list" ? "agents" : (p.name ?? p.sessionId ?? "subagent"),
        secondary: compact(p.description),
      };
    case "process":
      return {
        kind: `process · ${p.action}`,
        primary: p.target ?? "background work",
        mono: true,
      };
    case "mcp":
      return {
        kind: "MCP",
        primary: p.selectedToolName ?? t.toolName,
        secondary: p.serviceId,
      };
    default:
      return { kind: "", primary: t.toolName, secondary: compact(p.summary) };
  }
}

/* What a diff line is, in the words the runtime uses. The bridge writes
   "add" and "del" (crates/gents-desktop-bridge/src/snapshot/
   tool_presentation.rs, `diff_lines`), while the scenarios here were
   written as "added" and "removed" — so a test for 'added' alone sent
   every real line down the removed branch and a whole diff read as a
   deletion. Anything else is context, which is neither. */
export const diffKind = (kind: string): "added" | "removed" | "context" => {
  const k = kind.toLowerCase();
  if (k.startsWith("add") || k === "+") return "added";
  if (k.startsWith("del") || k.startsWith("rem") || k === "-") return "removed";
  return "context";
};

/* how big the change is, the way an editor says it: +18 −2 */
export function diffTally(diff: { kind: string; text?: string }[]) {
  let added = 0;
  let removed = 0;
  for (const line of diff) {
    const kind = diffKind(line.kind);
    if (kind === "added") added += 1;
    else if (kind === "removed") removed += 1;
  }
  if (!added && !removed) return null;
  return [added ? `+${added}` : null, removed ? `\u2212${removed}` : null]
    .filter(Boolean)
    .join(" ");
}

/* Reasoning a provider will not hand over.

   `ReasoningContent` has four shapes — Text, Summary, Encrypted, Redacted
   — and the runtime renders the last two as fixed placeholders rather than
   leak ciphertext: crates/gents-protocol/src/transcript.rs writes
   "[encrypted reasoning]" and "[redacted reasoning]". That is the right
   call upstream, but a transcript that offers it behind a disclosure
   promises something to read and delivers an apology.

   Matching the wording is sniffing a presentation detail: it is a constant
   in that crate today, and if it changes we silently start showing it
   again. The honest fix is a typed signal on the timeline item, which is
   filed with the other contract gaps in BACKGROUND-WORK.md. */
const WITHHELD = /^\s*\[(encrypted|redacted) reasoning\]\s*$/;

/* what of a turn's reasoning is actually readable */
export function readableReasoning(reasoning: string | null | undefined): string | null {
  if (!reasoning) return null;
  const kept = reasoning
    .split("\n")
    .filter((line) => !WITHHELD.test(line))
    .join("\n")
    .trim();
  return kept || null;
}

/* whether a turn had reasoning that the provider would not hand over: the
   fact is worth a quiet line, where the text itself is not */
export function reasoningWithheld(reasoning: string | null | undefined): boolean {
  if (!reasoning) return false;
  return reasoning.split("\n").some((line) => WITHHELD.test(line));
}

/* Every ending used to arrive as 'done', so a command that exited 1 was
   drawn exactly like one that exited 0 — same glyph, same muted ink, the
   exit code only inside the opened body. The outcome belongs on the row:
   a failure is the row a person is looking for, and a refusal is not a
   failure but the policy declining, which is why it reads as stopped.

   statusKind alone does not carry it. It is the call's lifecycle, and a
   command that exits non-zero still completes its lifecycle — the
   runtime puts that judgement on the presentation instead:

     failed = tool_status_is_error(tool) || meta.ok == false || timed_out
           || status == "exit_nonzero" || exit_code != 0

   (crates/.../tool_presentation.rs). So `completed` + `failed: true` is
   the ordinary shape of a test that did not pass, and reading only the
   lifecycle would mark nothing on the transcripts this is for. */
export const stepStatus = (tool: RenderedToolCallView): ToolStepStatus => {
  if (tool.denial) return "stopped";
  const kind = tool.statusKind;
  if (kind === "running") return "running";
  if (kind === "error") return "failed";
  /* the runtime's own call, for the case the lifecycle calls complete */
  if (tool.presentation.kind === "command" && tool.presentation.failed) return "failed";
  return kind === "success" ? "done" : "pending";
};
