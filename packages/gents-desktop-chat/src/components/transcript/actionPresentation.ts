import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

export type ActionEntry = {
  tool: RenderedToolCallView;
  launcher?: RenderedToolCallView;
};

export function actionStatus(tool: RenderedToolCallView): string {
  const status = tool.statusKind.toLowerCase();
  return status === "error" &&
    ["userCancelled", "interrupted", "unknown"].includes(
      tool.cancelCause?.cause ?? "",
    )
    ? "cancelled"
    : status;
}

export function actionItemKeys({ tool, launcher }: ActionEntry): string[] {
  return launcher ? [tool.itemKey, launcher.itemKey] : [tool.itemKey];
}

function object(value: string | null) {
  if (!value) return null;
  try {
    const parsed: unknown = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

export function actionEntries(tools: RenderedToolCallView[]): ActionEntry[] {
  const launches = new Map<string, RenderedToolCallView[]>();
  const targets = new Map<string, RenderedToolCallView[]>();
  const key = (request: string, call: string) =>
    JSON.stringify([request, call]);
  for (const tool of tools) {
    if (!tool.requestId) continue;
    if (tool.toolCallId) {
      const id = key(tool.requestId, tool.toolCallId);
      targets.set(id, [...(targets.get(id) ?? []), tool]);
    }
    const view = tool.presentation;
    if (
      tool.reconstruction.state !== "ready" ||
      tool.statusKind.toLowerCase() !== "success" ||
      view.kind !== "process" ||
      view.action !== "spawn"
    )
      continue;
    const id = object(view.output)?.tool_call_id;
    if (typeof id !== "string" || !id) continue;
    const target = key(tool.requestId, id);
    launches.set(target, [...(launches.get(target) ?? []), tool]);
  }
  const paired = new Map<RenderedToolCallView, RenderedToolCallView>();
  const hidden = new Set<RenderedToolCallView>();
  for (const [id, launchers] of launches) {
    const children = targets.get(id);
    if (launchers.length !== 1 || children?.length !== 1) continue;
    const launcher = launchers[0]!;
    const child = children[0]!;
    if (launcher === child || child.awaitMode !== "background" || child.denial)
      continue;
    paired.set(child, launcher);
    hidden.add(launcher);
  }
  return tools
    .filter((tool) => !hidden.has(tool))
    .map((tool) => ({
      tool,
      launcher: paired.get(tool),
    }));
}

export function toolLabel(name: string) {
  const builtin = name.split(".").at(-1);
  switch (builtin) {
    case "bash":
    case "bash_unrestricted":
    case "gents_exec":
    case "exec_command":
      return "Shell command";
    case "spawn_process":
      return "Start background action";
    case "wait_process":
      return "Wait for background action";
    case "read_process":
      return "Read background output";
    case "list_processes":
      return "List background actions";
    case "cancel_process":
      return "Stop background action";
    case "read_file":
      return "Read file";
    case "write_file":
      return "Write file";
    case "edit_file":
      return "Edit file";
    case "list_files":
      return "List files";
    case "glob":
      return "Find files";
    case "grep":
      return "Search files";
    default:
      return name;
  }
}

export function actionCommand({ tool, launcher }: ActionEntry): string | null {
  const view = tool.presentation;
  if (
    tool.reconstruction.state === "ready" &&
    view.kind === "command" &&
    view.command &&
    view.command !== tool.toolName
  )
    return view.command;
  const source = launcher ?? tool;
  if (
    source.reconstruction.state !== "ready" ||
    source.presentation.kind !== "process"
  )
    return null;
  const args = object(source.presentation.description);
  return typeof args?.command === "string" && args.command.trim()
    ? args.command
    : null;
}

export function actionTitle(entry: ActionEntry): string {
  const command = actionCommand(entry);
  if (command) return command;
  const { tool } = entry;
  if (tool.reconstruction.state !== "ready") return toolLabel(tool.toolName);
  const view = tool.presentation;
  switch (view.kind) {
    case "command":
      return toolLabel(tool.toolName);
    case "fileRead":
      return view.target ?? toolLabel(tool.toolName);
    case "fileEdit":
      return view.path ?? toolLabel(tool.toolName);
    case "agent":
      return view.name ?? "Agent";
    case "process":
      return toolLabel(tool.toolName);
    case "mcp":
      return view.selectedToolName ?? toolLabel(tool.toolName);
    case "generic":
      return view.summary ?? toolLabel(tool.toolName);
  }
}

export function actionKind(entry: ActionEntry): string | null {
  if (
    entry.tool.reconstruction.state === "ready" &&
    entry.tool.presentation.kind === "command"
  )
    return "command";
  if (actionCommand(entry)) return "command";
  const { tool } = entry;
  if (tool.reconstruction.state !== "ready") return null;
  switch (tool.presentation.kind) {
    case "fileRead":
      return toolLabel(tool.toolName).toLowerCase();
    case "fileEdit":
      return "edit";
    case "agent":
      return "agent";
    default:
      return null;
  }
}

export function runningAction(entry: ActionEntry) {
  const title = actionTitle(entry);
  const { tool } = entry;
  if (tool.reconstruction.state === "ready") {
    switch (tool.presentation.kind) {
      case "fileRead": {
        const operation = tool.presentation.operation;
        return `${operation === "grep" ? "Searching" : operation === "read_file" ? "Reading" : "Exploring"} ${title}`;
      }
      case "fileEdit":
        return `Editing ${title}`;
      case "agent":
        return tool.presentation.description ?? `Working with ${title}`;
    }
  }
  return `Running ${title}`;
}
