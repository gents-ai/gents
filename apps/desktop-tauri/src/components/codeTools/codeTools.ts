import type { RenderedToolCallView } from "../../lib/types";

// Code-aware projection of the agent's file/command tool calls. Everything is
// derived client-side from the tool args (raw JSON) + the runtime's result
// envelopes (`defra_exec: {json}` / `defra_fs: {json}` head line) that the
// transcript already carries — no bridge changes. File edits become diffs;
// bash calls become terminal blocks. The agent runs these ON ITS host, inside
// its tool-root/sandbox. Only successful, uncancelled calls are projected —
// failed/running/cancelled calls keep the generic disclosure (with its denial
// and cancel-cause rendering) so a failed edit never reads as an applied diff.
// The same honesty rule covers the result metadata itself: raw_json=true
// results are rescued from their bare-JSON shape, but a call whose metadata
// cannot be parsed at all (missing, malformed, or tail-truncated away) keeps
// the generic disclosure rather than projecting a fabricated ok badge.

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
 * Rescue a `raw_json=true` result: the runtime then emits one bare JSON
 * object with the same metadata fields flattened to the top level
 * (CommandOutput / WriteFileOutput / EditFileOutput all serde-flatten their
 * metadata). Returns null unless the whole result parses to an object
 * carrying the metadata's `ok`/`status` pair.
 */
function bareJsonMeta(result: string): Record<string, unknown> | null {
  const parsed = safeJsonObject(result);
  return parsed &&
    typeof parsed["ok"] === "boolean" &&
    typeof parsed["status"] === "string"
    ? parsed
    : null;
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

const STDOUT_PREFIX = "stdout:\n";
const STDERR_MARKER = "\nstderr:\n";
const EMPTY_PLACEHOLDER = "(empty)";

type StreamTruncation = { returnedBytes: number; truncated: boolean };

function streamTruncation(
  meta: Record<string, unknown> | null,
  key: string,
): StreamTruncation | null {
  const raw = meta?.[key];
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const returnedBytes = record["returned_bytes"];
  if (typeof returnedBytes !== "number") {
    return null;
  }
  return { returnedBytes, truncated: record["truncated"] === true };
}

/**
 * Parse the runtime's command body framing:
 * `stdout:\n<stdout-or-(empty)>\nstderr:\n<stderr-or-(empty)>`
 * (see render_command_output in toolset/shared/command.rs).
 *
 * The marker is appended after RAW stdout, so any stdout that itself contains
 * a bare "stderr:" line is ambiguous to a text search. The envelope's
 * `{stdout,stderr}_truncation.returned_bytes` gives an exact byte offset for
 * an untruncated stream (a truncated stream gains a variable-length
 * `[Showing lines …]` note, so its rendered length is not recoverable): try
 * the exact split anchored at stdout's end, then anchored at stderr's start
 * from the end of the body, each verified against the marker bytes. Fall back
 * to the first-occurrence text search only for legacy rows without metadata.
 */
function splitCommandStreams(
  body: string,
  stdoutTrunc: StreamTruncation | null,
  stderrTrunc: StreamTruncation | null,
): { stdout: string; stderr: string } {
  if (!body.startsWith(STDOUT_PREFIX)) {
    return { stdout: normalizeStream(body), stderr: "" };
  }
  const bytes = new TextEncoder().encode(body);
  if (stdoutTrunc && !stdoutTrunc.truncated) {
    const length =
      stdoutTrunc.returnedBytes > 0
        ? stdoutTrunc.returnedBytes
        : EMPTY_PLACEHOLDER.length;
    const split = splitAtMarker(bytes, STDOUT_PREFIX.length + length);
    if (split) {
      return split;
    }
  }
  if (stderrTrunc && !stderrTrunc.truncated) {
    const length =
      stderrTrunc.returnedBytes > 0
        ? stderrTrunc.returnedBytes
        : EMPTY_PLACEHOLDER.length;
    const split = splitAtMarker(bytes, bytes.length - length - STDERR_MARKER.length);
    if (split) {
      return split;
    }
  }
  const stderrAt = body.indexOf(STDERR_MARKER);
  if (stderrAt === -1) {
    return { stdout: normalizeStream(body.slice(STDOUT_PREFIX.length)), stderr: "" };
  }
  return {
    stdout: normalizeStream(body.slice(STDOUT_PREFIX.length, stderrAt)),
    stderr: normalizeStream(body.slice(stderrAt + STDERR_MARKER.length)),
  };
}

/** Split at a byte offset, verified against the literal marker bytes. */
function splitAtMarker(
  bytes: Uint8Array,
  markerStart: number,
): { stdout: string; stderr: string } | null {
  if (
    markerStart < STDOUT_PREFIX.length ||
    markerStart + STDERR_MARKER.length > bytes.length
  ) {
    return null;
  }
  const decoder = new TextDecoder();
  const marker = decoder.decode(
    bytes.subarray(markerStart, markerStart + STDERR_MARKER.length),
  );
  if (marker !== STDERR_MARKER) {
    return null;
  }
  return {
    stdout: normalizeStream(
      decoder.decode(bytes.subarray(STDOUT_PREFIX.length, markerStart)),
    ),
    stderr: normalizeStream(
      decoder.decode(bytes.subarray(markerStart + STDERR_MARKER.length)),
    ),
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
  const raw = tool.result?.rawText ?? "";
  const meta = splitEnvelope(raw, "defra_fs: ").meta ?? bareJsonMeta(raw);
  // No trustworthy metadata (missing, malformed, or truncated away): keep the
  // generic disclosure rather than guess at what was applied.
  if (!meta || meta["ok"] === false) {
    return null;
  }
  // Only write_file's metadata carries `created`; edit_file edits by contract.
  const created = name === "write_file" && meta["created"] === true;
  const replacementsRaw = meta["replacements_applied"];
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
  const raw = tool.result?.rawText ?? "";
  const envelope = splitEnvelope(raw, "defra_exec: ");
  let meta = envelope.meta;
  let streams: { stdout: string; stderr: string };
  if (meta) {
    streams = splitCommandStreams(
      envelope.body,
      streamTruncation(meta, "stdout_truncation"),
      streamTruncation(meta, "stderr_truncation"),
    );
  } else if ((meta = bareJsonMeta(raw))) {
    // raw_json=true: the streams are top-level JSON fields, not body framing.
    streams = {
      stdout: normalizeStream(stringField(meta, "stdout") ?? ""),
      stderr: normalizeStream(stringField(meta, "stderr") ?? ""),
    };
  } else {
    // No trustworthy envelope (missing, malformed, or truncated away): keep
    // the generic disclosure rather than fabricate an ok badge.
    return null;
  }
  // The envelope's `command` is the full shell-joined argv; the raw arg may be
  // just the executable when an args array was used.
  const command = stringField(meta, "command") ?? stringField(args, "command");
  if (!command) {
    return null;
  }
  const exitRaw = meta["exit_code"];
  const exitCode = typeof exitRaw === "number" ? exitRaw : null;
  const status = stringField(meta, "status");
  const timedOut = meta["timed_out"] === true || status === "timeout";
  const failed =
    (tool.statusKind ?? "").toLowerCase() === "error" ||
    meta["ok"] === false ||
    timedOut ||
    status === "exit_nonzero" ||
    (exitCode != null && exitCode !== 0);
  const { stdout, stderr } = streams;
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
