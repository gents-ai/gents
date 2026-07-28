import { stripAnsi, type DiffLine, type DiffLineKind } from "./types.js";

export function safeJsonObject(
  text?: string | null,
): Record<string, unknown> | null {
  if (!text) return null;
  try {
    const parsed: unknown = JSON.parse(text);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

export function stringField(
  record: Record<string, unknown> | null,
  key: string,
): string | null {
  const value = record?.[key];
  return typeof value === "string" ? value : null;
}

export function numberField(
  record: Record<string, unknown> | null,
  key: string,
): number | null {
  const value = record?.[key];
  return typeof value === "number" ? value : null;
}

export function bareJsonMeta(result: string): Record<string, unknown> | null {
  const parsed = safeJsonObject(result);
  return parsed &&
    typeof parsed["ok"] === "boolean" &&
    typeof parsed["status"] === "string"
    ? parsed
    : null;
}

export function splitEnvelope(
  result: string,
  prefix: string,
): { meta: Record<string, unknown> | null; body: string } {
  if (!result.startsWith(prefix)) {
    return { meta: null, body: result };
  }
  const newline = result.indexOf("\n");
  const head =
    newline === -1
      ? result.slice(prefix.length)
      : result.slice(prefix.length, newline);
  const body = newline === -1 ? "" : result.slice(newline + 1);
  return { meta: safeJsonObject(head), body };
}

const STDOUT_PREFIX = "stdout:\n";
const STDERR_MARKER = "\nstderr:\n";
const EMPTY_PLACEHOLDER = "(empty)";

export type StreamTruncation = {
  returnedBytes: number;
  truncated: boolean;
};

export function streamTruncation(
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

export function splitCommandStreams(
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
    if (split) return split;
  }
  if (stderrTrunc && !stderrTrunc.truncated) {
    const length =
      stderrTrunc.returnedBytes > 0
        ? stderrTrunc.returnedBytes
        : EMPTY_PLACEHOLDER.length;
    const split = splitAtMarker(
      bytes,
      bytes.length - length - STDERR_MARKER.length,
    );
    if (split) return split;
  }
  const stderrAt = body.indexOf(STDERR_MARKER);
  if (stderrAt === -1) {
    return {
      stdout: normalizeStream(body.slice(STDOUT_PREFIX.length)),
      stderr: "",
    };
  }
  return {
    stdout: normalizeStream(body.slice(STDOUT_PREFIX.length, stderrAt)),
    stderr: normalizeStream(body.slice(stderrAt + STDERR_MARKER.length)),
  };
}

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
  if (marker !== STDERR_MARKER) return null;
  return {
    stdout: normalizeStream(
      decoder.decode(bytes.subarray(STDOUT_PREFIX.length, markerStart)),
    ),
    stderr: normalizeStream(
      decoder.decode(bytes.subarray(markerStart + STDERR_MARKER.length)),
    ),
  };
}

export function normalizeStream(value: string): string {
  const trimmed = stripAnsi(value).replace(/\s+$/, "");
  return trimmed === EMPTY_PLACEHOLDER ? "" : trimmed;
}

export function toDiffLines(text: string, kind: DiffLineKind): DiffLine[] {
  return text
    .replace(/\r?\n$/, "")
    .split(/\r?\n/)
    .map((line) => ({ kind, text: line }));
}
