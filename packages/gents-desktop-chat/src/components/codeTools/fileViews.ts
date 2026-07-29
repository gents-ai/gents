import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

import {
  bareJsonMeta,
  numberField,
  safeJsonObject,
  splitEnvelope,
  stringField,
  toDiffLines,
} from "./parsing.js";
import type {
  DiffLine,
  FileEditView,
  FileReadTool,
  FileReadView,
} from "./types.js";

export function toFileReadView(
  tool: RenderedToolCallView,
  name: FileReadTool,
): FileReadView | null {
  const { meta, body } = splitEnvelope(
    tool.result?.rawText ?? "",
    "gents_fs: ",
  );
  if (!meta) return null;
  const args = safeJsonObject(tool.args?.rawText);
  const target =
    stringField(meta, "path") ??
    stringField(args, "path") ??
    stringField(args, "pattern") ??
    null;
  return {
    kind: "fileRead",
    tool: name,
    target,
    returnedCount: numberField(meta, "returned_count"),
    totalCount: numberField(meta, "total_count"),
    truncated: meta["truncated"] === true,
    body: body.replace(/\s+$/, ""),
  };
}

export function toFileEditView(
  tool: RenderedToolCallView,
  name: string,
): FileEditView | null {
  const args = safeJsonObject(tool.args?.rawText);
  const path = stringField(args, "path");
  if (!path) return null;
  const raw = tool.result?.rawText ?? "";
  const meta = splitEnvelope(raw, "gents_fs: ").meta ?? bareJsonMeta(raw);
  if (!meta || meta["ok"] === false) return null;

  const created = name === "write_file" && meta["created"] === true;
  const overwrite = name === "write_file" && meta["created"] === false;
  const replacementsRaw = meta["replacements_applied"];
  const replacementsApplied =
    typeof replacementsRaw === "number" && replacementsRaw > 0
      ? replacementsRaw
      : 1;
  let diff: DiffLine[];
  if (name === "write_file") {
    diff = toDiffLines(stringField(args, "content") ?? "", "add");
  } else {
    diff = [
      ...toDiffLines(stringField(args, "old_text") ?? "", "del"),
      ...toDiffLines(stringField(args, "new_text") ?? "", "add"),
    ];
  }
  return {
    kind: "fileEdit",
    path,
    created,
    overwrite,
    replacementsApplied,
    diff,
  };
}
