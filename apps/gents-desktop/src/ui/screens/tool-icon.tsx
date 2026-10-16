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

const BY_NAME: Record<string, Glyph> = {
  grep: Search,
  glob: Search,
  metrics: Search,
  read_doc: FileText,
};
const BY_KIND: Record<string, Glyph> = {
  command: Terminal,
  fileRead: FileText,
  fileEdit: PenLine,
  agent: Bot,
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
