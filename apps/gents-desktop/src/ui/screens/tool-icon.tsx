/* One glyph per kind of tool call, so the transcript and the trace mark a
   call the same way. The contract's presentation kind decides it, with a
   few tool names refining what a kind cannot say on its own: a grep and a
   read_file are both fileRead, and they do not look like the same act.
   Plain Lucide names, no kind aliases — the Figma library mirrors these. */
import {
  Bot,
  Plug,
  FileText,
  PenLine,
  Search,
  SquareTerminal,
  Terminal,
  Wrench,
} from "lucide-react";
import type { ComponentType } from "react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

type Glyph = ComponentType<{ className?: string }>;

/* A tool name is more specific than its kind, where the name is known.
   Kept short on purpose: every entry here is a claim that the contract's
   own kind is not enough, and a name nothing calls is a claim about
   nothing. reason, plan and delegate were carried over from an older map
   and are covered by their kinds. */
const BY_NAME: Record<string, Glyph> = {
  grep: Search,
  glob: Search,
  metrics: Search,
  read_doc: FileText,
};
/* The config's own vocabulary, so a thing looks the same wherever it is
   named: Bot is an agent, Wrench is a tool, Plug is a tool service.
   Play belongs to a task and is not borrowed here for a background
   process — a long-running command keeps the terminal, squared off. */
const BY_KIND: Record<string, Glyph> = {
  command: Terminal,
  fileRead: FileText,
  fileEdit: PenLine,
  subagent: Bot,
  process: SquareTerminal,
  mcp: Plug,
  generic: Wrench,
};

function toolGlyph(tool: RenderedToolCallView): Glyph {
  const name = (tool.toolName ?? "").split(".").pop() ?? "";
  return BY_NAME[name] ?? BY_KIND[tool.presentation.kind] ?? Wrench;
}

export function ToolIcon({
  tool,
  className,
}: {
  tool: RenderedToolCallView;
  className?: string;
}) {
  const Glyph = toolGlyph(tool);
  return <Glyph className={className ?? "size-4"} />;
}
