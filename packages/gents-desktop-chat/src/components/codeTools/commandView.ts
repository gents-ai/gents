import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

import {
  bareJsonMeta,
  normalizeStream,
  safeJsonObject,
  splitCommandStreams,
  splitEnvelope,
  streamTruncation,
  stringField,
} from "./parsing.js";
import type { CommandRunView } from "./types.js";

export function toCommandRunView(
  tool: RenderedToolCallView,
): CommandRunView | null {
  const args = safeJsonObject(tool.args?.rawText);
  const raw = tool.result?.rawText ?? "";
  const envelope = splitEnvelope(raw, "gents_exec: ");
  let meta = envelope.meta;
  let streams: { stdout: string; stderr: string };
  if (meta) {
    streams = splitCommandStreams(
      envelope.body,
      streamTruncation(meta, "stdout_truncation"),
      streamTruncation(meta, "stderr_truncation"),
    );
  } else if ((meta = bareJsonMeta(raw))) {
    streams = {
      stdout: normalizeStream(stringField(meta, "stdout") ?? ""),
      stderr: normalizeStream(stringField(meta, "stderr") ?? ""),
    };
  } else {
    return null;
  }

  const command = stringField(meta, "command") ?? stringField(args, "command");
  if (!command) return null;
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
  const durationRaw = meta["duration_ms"];
  return {
    kind: "command",
    command,
    exitCode,
    executionMode: stringField(meta, "execution_mode"),
    networkMode: stringField(meta, "network_mode"),
    durationMs: typeof durationRaw === "number" ? durationRaw : null,
    cwd: stringField(meta, "cwd"),
    timedOut,
    failed,
    stdout: streams.stdout,
    stderr: streams.stderr,
  };
}
