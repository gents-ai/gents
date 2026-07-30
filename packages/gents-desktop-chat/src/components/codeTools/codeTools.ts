import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

import { toCommandRunView } from "./commandView.js";
import { toFileEditView, toFileReadView } from "./fileViews.js";
import type { CodeToolView, FileReadTool } from "./types.js";

export * from "./types.js";

const FILE_EDIT_TOOLS = new Set(["write_file", "edit_file"]);
const COMMAND_TOOLS = new Set(["bash", "bash_unrestricted"]);
const FILE_READ_TOOLS = new Set<string>([
  "read_file",
  "grep",
  "glob",
  "list_files",
]);

export function toCodeToolView(
  tool: RenderedToolCallView,
): CodeToolView | null {
  const name = tool.toolName.toLowerCase();
  if (FILE_EDIT_TOOLS.has(name)) {
    return toFileEditView(tool, name);
  }
  if (COMMAND_TOOLS.has(name)) {
    return toCommandRunView(tool);
  }
  if (FILE_READ_TOOLS.has(name)) {
    return toFileReadView(tool, name as FileReadTool);
  }
  return null;
}
