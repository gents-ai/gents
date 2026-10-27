import type {
  RenderedToolCallView,
  ToolDiffLineKind,
  ToolDiffLineView,
  ToolPresentationView,
} from "@source-inc/gents-desktop-client";
import type { ToolStepStatus } from "@gents/ui/conversation";
import { shortPath } from "../lib/paths";

const toolPath = (path: string) => shortPath(path, { keep: 3 });

const ASSIGNMENTS = /^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S*)(?:\s+|$))+/;

export const spoken = (command: string): string => {
  const parts = command
    .split("&&")
    .map((part) => part.trim().replace(ASSIGNMENTS, "").trim())
    .filter((part) => part && !/^cd\b/.test(part));
  return parts.length ? parts.join(" && ") : command.trim();
};

const DIFF_MARK: Record<ToolDiffLineKind, string> = {
  added: "+",
  removed: "-",
  context: " ",
};

export const diffText = (diff: ToolDiffLineView[]) =>
  diff.map((line) => `${DIFF_MARK[line.kind]}${line.text}`).join("\n");

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

export function toolSummary(t: RenderedToolCallView): {
  kind: string;
  primary: string;
  full?: string;
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
        primary: p.target ? toolPath(p.target) : t.toolName,
        full: p.target ?? undefined,
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
        primary: p.path ? toolPath(p.path) : t.toolName,
        full: p.path ?? undefined,
        secondary:
          p.replacementsApplied != null && p.replacementsApplied > 1
            ? `×${p.replacementsApplied}`
            : null,
        mono: true,
      };
    }
    case "agent":
      return {
        kind: `agent · ${p.action}`,
        primary:
          p.action === "list"
            ? "agents"
            : p.action === "sessions"
              ? "sessions"
              : (p.name ?? p.sessionId ?? "agent"),
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

export function diffTally(diff: readonly ToolDiffLineView[]) {
  let added = 0;
  let removed = 0;
  for (const line of diff) {
    if (line.kind === "added") added += 1;
    else if (line.kind === "removed") removed += 1;
  }
  if (!added && !removed) return null;
  return [added ? `+${added}` : null, removed ? `\u2212${removed}` : null]
    .filter(Boolean)
    .join(" ");
}

const WITHHELD = /^\s*\[(encrypted|redacted) reasoning\]\s*$/;

export function readableReasoning(reasoning: string | null | undefined): string | null {
  if (!reasoning) return null;
  const kept = reasoning
    .split("\n")
    .filter((line) => !WITHHELD.test(line))
    .join("\n")
    .trim();
  return kept || null;
}

export function reasoningWithheld(reasoning: string | null | undefined): boolean {
  if (!reasoning) return false;
  return reasoning.split("\n").some((line) => WITHHELD.test(line));
}

export const stepStatus = (tool: RenderedToolCallView): ToolStepStatus => {
  if (tool.denial) return "stopped";
  const kind = tool.statusKind;
  if (kind === "running") return "running";
  if (kind === "error") return "failed";
  if (tool.presentation.kind === "command" && tool.presentation.failed) return "failed";
  return kind === "success" ? "done" : "pending";
};
