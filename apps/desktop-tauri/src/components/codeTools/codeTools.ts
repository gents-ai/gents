import type { RenderedToolCallView } from "../../lib/types";

// Code-aware projection of the agent's file/command tool calls. Everything is
// derived client-side from the tool args (raw JSON) + the runtime's result
// envelopes (`defra_exec: {json}` / `defra_fs: {json}` head line) that the
// transcript already carries — no bridge changes. File edits become diffs;
// bash calls become terminal blocks. The agent runs these ON ITS host, inside
// its tool-root/sandbox. Only successful, uncancelled calls are projected —
// failed/running/cancelled calls keep the generic disclosure (with its denial
// and cancel-cause rendering) so a failed edit never reads as an applied diff.

export type DiffLineKind = "add" | "del";
export type DiffLine = { kind: DiffLineKind; text: string };

export type FileEditView = {
  kind: "fileEdit";
  path: string;
  created: boolean;
  /** >1 when edit_file replace_all touched multiple sites. */
  replacementsApplied: number;
  diff: DiffLine[];
};

export type CommandRunView = {
  kind: "command";
  command: string;
  exitCode: number | null;
  executionMode: string | null;
  networkMode: string | null;
  timedOut: boolean;
  failed: boolean;
  stdout: string;
  stderr: string;
};

export type CodeToolView = FileEditView | CommandRunView;

const FILE_EDIT_TOOLS = new Set(["write_file", "edit_file"]);
const COMMAND_TOOLS = new Set(["bash", "bash_unrestricted"]);

function safeJsonObject(text?: string | null): Record<string, unknown> | null {
  if (!text) {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(text);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function stringField(
  record: Record<string, unknown> | null,
  key: string,
): string | null {
  const value = record?.[key];
  return typeof value === "string" ? value : null;
}

/**
 * Split a `<prefix>{json}\n<body>` metadata envelope (defra_exec / defra_fs)
 * into its parsed JSON head and the remaining output body.
 */
function splitEnvelope(
  result: string,
  prefix: string,
): { meta: Record<string, unknown> | null; body: string } {
  if (!result.startsWith(prefix)) {
    return { meta: null, body: result };
  }
  const newline = result.indexOf("\n");
  const head =
    newline === -1 ? result.slice(prefix.length) : result.slice(prefix.length, newline);
  const body = newline === -1 ? "" : result.slice(newline + 1);
  return { meta: safeJsonObject(head), body };
}

/**
 * Parse the runtime's command body framing:
 * `stdout:\n<stdout-or-(empty)>\nstderr:\n<stderr-or-(empty)>`
 * (see render_command_output in toolset/shared/command.rs). Falls back to
 * treating the whole body as stdout when the framing is absent.
 */
function splitCommandStreams(body: string): { stdout: string; stderr: string } {
  const stdoutPrefix = "stdout:\n";
  const stderrMarker = "\nstderr:\n";
  if (!body.startsWith(stdoutPrefix)) {
    return { stdout: normalizeStream(body), stderr: "" };
  }
  const stderrAt = body.indexOf(stderrMarker);
  if (stderrAt === -1) {
    return { stdout: normalizeStream(body.slice(stdoutPrefix.length)), stderr: "" };
  }
  return {
    stdout: normalizeStream(body.slice(stdoutPrefix.length, stderrAt)),
    stderr: normalizeStream(body.slice(stderrAt + stderrMarker.length)),
  };
}

function normalizeStream(value: string): string {
  const trimmed = value.replace(/\s+$/, "");
  return trimmed === "(empty)" ? "" : trimmed;
}

/** Split file text into diff lines, tolerating CRLF and a trailing newline. */
function toDiffLines(text: string, kind: DiffLineKind): DiffLine[] {
  return text
    .replace(/\r?\n$/, "")
    .split(/\r?\n/)
    .map((line) => ({
      kind,
      text: line,
    }));
}

/** Project a code tool call into a diff/terminal view, or null if not one. */
export function toCodeToolView(tool: RenderedToolCallView): CodeToolView | null {
  const name = tool.toolName.toLowerCase();
  if (FILE_EDIT_TOOLS.has(name)) {
    return toFileEditView(tool, name);
  }
  if (COMMAND_TOOLS.has(name)) {
    return toCommandRunView(tool);
  }
  return null;
}

function toFileEditView(tool: RenderedToolCallView, name: string): FileEditView | null {
  const args = safeJsonObject(tool.args?.rawText);
  const path = stringField(args, "path");
  if (!path) {
    return null;
  }
  const { meta } = splitEnvelope(tool.result?.rawText ?? "", "defra_fs: ");
  // Only write_file's metadata carries `created`; edit_file edits by contract.
  const created = name === "write_file" && meta?.["created"] === true;
  const replacementsRaw = meta?.["replacements_applied"];
  const replacementsApplied =
    typeof replacementsRaw === "number" && replacementsRaw > 0 ? replacementsRaw : 1;
  let diff: DiffLine[];
  if (name === "write_file") {
    diff = toDiffLines(stringField(args, "content") ?? "", "add");
  } else {
    diff = [
      ...toDiffLines(stringField(args, "old_text") ?? "", "del"),
      ...toDiffLines(stringField(args, "new_text") ?? "", "add"),
    ];
  }
  return { kind: "fileEdit", path, created, replacementsApplied, diff };
}

function toCommandRunView(tool: RenderedToolCallView): CommandRunView | null {
  const args = safeJsonObject(tool.args?.rawText);
  const { meta, body } = splitEnvelope(tool.result?.rawText ?? "", "defra_exec: ");
  // The envelope's `command` is the full shell-joined argv; the raw arg may be
  // just the executable when an args array was used.
  const command = stringField(meta, "command") ?? stringField(args, "command");
  if (!command) {
    return null;
  }
  const exitRaw = meta?.["exit_code"];
  const exitCode = typeof exitRaw === "number" ? exitRaw : null;
  const status = stringField(meta, "status");
  const timedOut = meta?.["timed_out"] === true || status === "timeout";
  const failed =
    (tool.statusKind ?? "").toLowerCase() === "error" ||
    meta?.["ok"] === false ||
    timedOut ||
    status === "exit_nonzero" ||
    (exitCode != null && exitCode !== 0);
  const { stdout, stderr } = splitCommandStreams(body);
  return {
    kind: "command",
    command,
    exitCode,
    executionMode: stringField(meta, "execution_mode"),
    networkMode: stringField(meta, "network_mode"),
    timedOut,
    failed,
    stdout,
    stderr,
  };
}
