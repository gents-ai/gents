import type {
  DesktopApiAdapter,
  FileChangeImages,
  FileImage,
  ToolFileChanges,
} from "@source-inc/gents-desktop-client";
import type { ChangedFile, FileEdit } from "../ui/lib/turnChanges";

/** Core keeps a copy only up to this size. */
const MAX_IMAGE_BYTES = 1024 * 1024;

/** The session a turn's edits ran in. */
export type SessionScope = { nodeDid: string; sessionId: string };

/** One file's versions to compare: null where the file did not exist. */
export type RecordedDiff =
  | { kind: "text"; before: string | null; after: string | null; partial: boolean }
  | { kind: "unavailable"; reason: string };

class Unavailable extends Error {}

/** How the bridge says the node holding the copies could not be reached:
    it is offline, or did not answer in time. Its wording tells the reader
    what to do, so it is shown as it is. */
const UNREACHABLE = [
  /^Reconnect this runtime/,
  /^File review is waiting for the runtime/,
];

export const isUnreachable = (error: unknown): error is Error =>
  error instanceof Error && UNREACHABLE.some((pattern) => pattern.test(error.message));

const hex = (bytes: ArrayBuffer) =>
  [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");

/** A recorded version as text, checked against the hash core recorded with
    it, so a damaged or swapped copy is never shown as what the edit did. */
export async function imageText(image: FileImage): Promise<string | null> {
  if (image.kind === "missing") return null;
  if (image.kind !== "file")
    throw new Unavailable("A recorded version is unavailable.");
  if (image.data.length > Math.ceil(MAX_IMAGE_BYTES / 3) * 4)
    throw new Unavailable("This file is too large to show.");
  const bytes = Uint8Array.from(atob(image.data), (c) => c.charCodeAt(0));
  const digest = `sha256:${hex(await crypto.subtle.digest("SHA-256", bytes))}`;
  if (digest !== image.content_hash)
    throw new Unavailable("A recorded version could not be verified.");
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw new Unavailable("This file is not text.");
  }
  if (text.includes("\0")) throw new Unavailable("This file is not text.");
  return text;
}

const trimEnd = (path: string) => path.replace(/[\\/]+$/, "");

/** Whether a recorded change is the file the tool reported, which may be
    given absolute or relative to the tool's root. */
export const isReported = (change: FileChangeImages, reported: string) =>
  change.path === reported || `${trimEnd(change.root)}/${change.path}` === reported;

const sameImage = (a: FileImage, b: FileImage) =>
  a.kind === b.kind &&
  (a.kind !== "file" || (b.kind === "file" && a.content_hash === b.content_hash));

/** What a turn did to one file: from before its first edit to after its
    last when each edit started where the previous one ended; otherwise
    something else changed the file in between, and only the last edit is
    shown, marked partial. */
export async function recordedDiff(
  fetch: NonNullable<DesktopApiAdapter["fetchToolFileChanges"]>,
  scope: SessionScope,
  file: ChangedFile,
): Promise<RecordedDiff> {
  try {
    const changes: FileChangeImages[] = [];
    for (const edit of file.edits) changes.push(await changeOf(fetch, scope, edit));
    const chained = changes.every(
      (c, i) => i === 0 || sameImage(changes[i - 1]!.after, c.before),
    );
    const first = chained ? changes[0]! : changes[changes.length - 1]!;
    const last = changes[changes.length - 1]!;
    return {
      kind: "text",
      before: await imageText(first.before),
      after: await imageText(last.after),
      partial: !chained,
    };
  } catch (error) {
    if (isUnreachable(error)) return { kind: "unavailable", reason: error.message };
    return {
      kind: "unavailable",
      reason:
        error instanceof Unavailable
          ? error.message
          : "The recorded versions could not be read.",
    };
  }
}

async function changeOf(
  fetch: NonNullable<DesktopApiAdapter["fetchToolFileChanges"]>,
  scope: SessionScope,
  edit: FileEdit,
): Promise<FileChangeImages> {
  const args = {
    ...scope,
    requestId: edit.requestId,
    toolCallDocId: edit.toolCallDocId,
  };
  /* the bridge also answers null while its view of the request is being
     resynced, so one more read separates that from an edit with no record */
  const recorded: ToolFileChanges | null = (await fetch(args)) ?? (await fetch(args));
  const change = recorded?.changes.find((c) => isReported(c, edit.path));
  if (!change) throw new Unavailable("This edit recorded no file versions.");
  return change;
}
