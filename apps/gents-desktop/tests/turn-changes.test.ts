import type {
  FileImage,
  RenderedTimelineItem,
  RenderedToolCallView,
  ToolFileChanges,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import { excerptPatch } from "@/lib/excerptPatch";
import { fileId, preferFile, queued } from "../src/lib/fileReads";
import { patchText } from "@/lib/filePatch";
import { recordedDiff } from "../src/lib/recordedFiles";
import {
  relativeTo,
  sameChangedFiles,
  sharedFolder,
  turnChanges,
} from "@/lib/turnChanges";
import { groupTranscript } from "@/screens/transcript-groups";
import { assistantMessage, userMessage } from "./timeline-fixture";

let seq = 0;
const edit = (
  path: string,
  { applied = true, added = 1, removed = 0 } = {},
): RenderedToolCallView =>
  ({
    itemKey: `t${++seq}`,
    toolName: "edit_file",
    statusKind: "success",
    requestId: "r1",
    toolCallDocId: `call-${seq}`,
    presentation: {
      kind: "fileEdit",
      operation: "edit_file",
      path,
      applied,
      created: false,
      replacementsApplied: 1,
      diff: [
        ...Array.from({ length: added }, () => ({ kind: "added", text: "+" })),
        ...Array.from({ length: removed }, () => ({ kind: "removed", text: "-" })),
      ],
      fallbackOutput: null,
    },
  }) as unknown as RenderedToolCallView;

const group = (n: number, ...tools: RenderedToolCallView[]): RenderedTimelineItem => ({
  kind: "toolGroup",
  itemKey: `g${n}`,
  messageSequence: n,
  tools,
});
const said = (n: number): RenderedTimelineItem =>
  assistantMessage({
    kind: "assistantMessage",
    itemKey: `a${n}`,
    sequence: n,
    content: "Done.",
    reasoning: null,
    timestamp: null,
  });
const person = (n: number): RenderedTimelineItem =>
  userMessage({
    kind: "userMessage",
    itemKey: `u${n}`,
    sequence: n,
    content: "go",
    timestamp: null,
  });

describe("turn changes", () => {
  it("collects a turn's applied edits by file under its final reply", () => {
    const entries = groupTranscript([
      person(1),
      group(2, edit("src/a.rs", { added: 2 }), edit("src/b.rs", { applied: false })),
      group(3, edit("src/a.rs", { added: 0, removed: 1 }), edit("src/c.rs")),
      said(4),
      person(5),
      group(6, edit("src/d.rs")),
      said(7),
    ]);
    const changes = turnChanges(entries, new Set(["a4", "a7"]));
    expect(
      changes
        .get("a4")!
        .map(({ path, added, removed, edits }) => [path, added, removed, edits.length]),
    ).toEqual([
      ["src/a.rs", 2, 1, 2],
      ["src/c.rs", 1, 0, 1],
    ]);
    expect(changes.get("a7")!.map((f) => f.path)).toEqual(["src/d.rs"]);
  });

  it("gives a turn without a final reply no changes", () => {
    const entries = groupTranscript([person(1), group(2, edit("src/a.rs"))]);
    expect(turnChanges(entries, new Set()).size).toBe(0);
  });

  it("knows a turn's files again when they are worked out anew", () => {
    const first = edit("src/a.rs");
    const filesOf = (...tools: RenderedToolCallView[]) =>
      turnChanges(
        groupTranscript([person(1), group(2, ...tools), said(3)]),
        new Set(["a3"]),
      ).get("a3")!;
    const files = filesOf(first);
    expect(sameChangedFiles(files, filesOf(first))).toBe(true);
    expect(sameChangedFiles(files, filesOf(first, edit("src/a.rs")))).toBe(false);
  });

  it("names files below the deepest folder they share", () => {
    const paths = ["/p/snake/README.md", "/p/snake/src/game.js", "/p/snakes/x.md"];
    expect(sharedFolder(paths.slice(0, 2))).toBe("/p/snake");
    expect(relativeTo("/p/snake", paths[1]!)).toBe("src/game.js");
    expect(sharedFolder(paths)).toBe("/p");
    expect(sharedFolder(["README.md", "src/a.rs"])).toBe("");
  });
});

const bytes = (text: string) => new TextEncoder().encode(text);
async function image(text: string): Promise<FileImage> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes(text)));
  return {
    kind: "file",
    content_hash: `sha256:${[...digest].map((b) => b.toString(16).padStart(2, "0")).join("")}`,
    data: btoa(String.fromCharCode(...bytes(text))),
    permissions: { kind: "unix", mode: 0o644 } as never,
    links: 1,
  };
}

function recorder(byCall: Record<string, [FileImage, FileImage]>) {
  return async ({
    toolCallDocId,
  }: {
    toolCallDocId: string;
  }): Promise<ToolFileChanges> => ({
    version: 1,
    tool_call_doc_id: toolCallDocId,
    changes: [
      {
        root: "/work",
        path: "src/a.rs",
        before: byCall[toolCallDocId]![0],
        after: byCall[toolCallDocId]![1],
      },
    ],
  });
}

const SCOPE = { nodeDid: "did:node", sessionId: "s" };

const fileOf = (...calls: string[]) => ({
  path: "/work/src/a.rs",
  added: 0,
  removed: 0,
  edits: calls.map((toolCallDocId) => ({
    path: "/work/src/a.rs",
    requestId: "r1",
    toolCallDocId,
    added: 0,
    removed: 0,
  })),
});

describe("recorded diff", () => {
  it("spans a file's edits when each starts where the last ended", async () => {
    const [v1, v2, v3] = await Promise.all([
      image("one"),
      image("two"),
      image("three"),
    ]);
    const fetch = recorder({ c1: [v1, v2], c2: [v2, v3] });
    expect(await recordedDiff(fetch, SCOPE, fileOf("c1", "c2"))).toEqual({
      kind: "text",
      before: "one",
      after: "three",
      partial: false,
    });
  });

  it("shows only the last edit when something else changed the file between", async () => {
    const [v1, v2, v3, v4] = await Promise.all(
      ["one", "two", "other", "four"].map(image),
    );
    const fetch = recorder({ c1: [v1!, v2!], c2: [v3!, v4!] });
    expect(await recordedDiff(fetch, SCOPE, fileOf("c1", "c2"))).toMatchObject({
      before: "other",
      after: "four",
      partial: true,
    });
  });

  it("refuses a copy that does not match its recorded hash", async () => {
    const [v1, v2] = await Promise.all([image("one"), image("two")]);
    const fetch = recorder({
      c1: [
        {
          ...(v1 as Extract<FileImage, { kind: "file" }>),
          content_hash: (v2 as { content_hash: string }).content_hash,
        },
        v2,
      ],
    });
    expect(await recordedDiff(fetch, SCOPE, fileOf("c1"))).toEqual({
      kind: "unavailable",
      reason: "A recorded version could not be verified.",
    });
  });

  it("takes an answer without a record as an edit that recorded none, in one read", async () => {
    let calls = 0;
    const fetch: Parameters<typeof recordedDiff>[0] = async () => {
      calls += 1;
      return null as never;
    };
    expect(await recordedDiff(fetch, SCOPE, fileOf("c1"))).toEqual({
      kind: "unavailable",
      reason: "This edit recorded no file versions.",
    });
    expect(calls).toBe(1);
  });

  it("treats a missing side as a created file", async () => {
    const fetch = recorder({ c1: [{ kind: "missing" }, await image("new")] });
    expect(await recordedDiff(fetch, SCOPE, fileOf("c1"))).toMatchObject({
      before: null,
      after: "new",
    });
  });
});

describe("patch text", () => {
  it("writes the change as a unified patch, a created file against nothing", () => {
    expect(patchText("a.rs", "one\ntwo\n", "one\nthree\n")).toBe(
      "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n",
    );
    expect(patchText("b.rs", null, "new\n")).toBe(
      "--- /dev/null\n+++ b/b.rs\n@@ -0,0 +1,1 @@\n+new\n",
    );
  });
});

describe("excerpt patch", () => {
  it("starts both sides at the excerpt's first line, and a new file from nothing", () => {
    const numbered = excerptPatch("a.rs", [
      { kind: "context", text: "impl A {", lineNumber: 3 },
      { kind: "removed", text: "old", lineNumber: 4 },
      { kind: "added", text: "new", lineNumber: 4 },
      { kind: "added", text: "more", lineNumber: 5 },
    ]);
    expect(numbered.split("\n").slice(2, 7)).toEqual([
      "@@ -3,2 +3,3 @@",
      " impl A {",
      "-old",
      "+new",
      "+more",
    ]);
    expect(excerptPatch("b.rs", [{ kind: "added", text: "fn b() {}" }])).toContain(
      "@@ -0,0 +1,1 @@",
    );
  });
});

describe("file reads", () => {
  it("reads the chosen file before the others waiting", async () => {
    const order: string[] = [];
    const fetch = async ({ toolCallDocId }: { toolCallDocId: string }) => {
      order.push(toolCallDocId);
      return null;
    };
    const read = (path: string) =>
      queued(
        fetch as never,
        fileId(SCOPE, path),
      )({
        ...SCOPE,
        requestId: "r1",
        toolCallDocId: path,
      });
    preferFile(SCOPE, "c.rs");
    await Promise.all(["a.rs", "b.rs", "c.rs"].map(read));
    expect(order).toEqual(["c.rs", "a.rs", "b.rs"]);
  });
});
