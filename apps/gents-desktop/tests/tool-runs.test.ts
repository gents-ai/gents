/* The fold is the one piece of the transcript that is ours rather than the
   desktop's: it decides how a long run of tool calls reads. Its rules were
   measured against a real export (1,989 calls) and several of them exist
   because a cheaper rule was wrong on that data, so they are pinned here.

   A pure module over RenderedToolCallView, so it is tested here beside the
   code rather than in the kit: the kit deliberately knows nothing of the
   desktop client, and this type comes from it. */
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import { firstFailure, foldWorkers, shortPath, workerStory } from "@/screens/tool-runs";

let seq = 0;
const call = (
  presentation: RenderedToolCallView["presentation"],
  statusKind = "completed",
): RenderedToolCallView => ({
  itemKey: `t${++seq}`,
  toolName: "tool",
  statusKind,
  presentation,
});

const cmd = (command: string, status = "completed", failed = false) =>
  call(
    {
      kind: "command",
      command,
      exitCode: failed ? 1 : 0,
      timedOut: false,
      failed,
      durationMs: 10,
      cwd: null,
      executionMode: null,
      networkMode: null,
      stdout: "",
      stderr: "",
      fallbackOutput: null,
    },
    status,
  );

const read = (target: string, status = "completed") =>
  call(
    {
      kind: "fileRead",
      operation: "read",
      target,
      returnedCount: null,
      totalCount: null,
      truncated: false,
      body: "",
      fallbackOutput: null,
    },
    status,
  );

const edit = (path: string, status = "completed") =>
  call(
    {
      kind: "fileEdit",
      operation: "replace",
      path,
      created: false,
      replacementsApplied: 1,
      diff: [],
      fallbackOutput: null,
    },
    status,
  );

const worker = (action: string, name: string, sessionId: string | null = null) =>
  call({
    kind: "subagent",
    action,
    name,
    sessionId,
    description: null,
    output: null,
  });

const runs = (tools: RenderedToolCallView[]) => foldWorkers(tools);
const kinds = (tools: RenderedToolCallView[]) => runs(tools).map((r) => r.kind);
const labels = (tools: RenderedToolCallView[]) =>
  runs(tools).map((r) => (r.kind === "run" ? r.label : r.kind));

describe("shortPath", () => {
  it("keeps a relative path whole", () => {
    expect(shortPath("src/api/export.rs")).toBe("src/api/export.rs");
  });

  it("shortens an absolute POSIX path to its last segments", () => {
    expect(shortPath("/home/dev/work/src/api/export.rs")).toBe("…/src/api/export.rs");
  });

  /* a Windows path is not a POSIX path with odd separators: splitting on
     the wrong one produced one segment and left the whole path in the row */
  it("shortens a Windows path with its own separator", () => {
    expect(shortPath("C:\\Users\\dev\\src\\api\\export.rs")).toBe(
      "…\\src\\api\\export.rs",
    );
  });

  it("leaves a path already short enough", () => {
    expect(shortPath("/etc/hosts")).toBe("/etc/hosts");
  });
});

describe("folding a run", () => {
  it("folds three reads into one row that counts them", () => {
    const out = runs([read("a.rs"), read("b.rs"), read("c.rs")]);
    expect(out).toHaveLength(1);
    expect(out[0]!.kind).toBe("run");
    if (out[0]!.kind === "run") expect(out[0]!.label).toMatch(/3 files/);
  });

  it("leaves two reads alone, below the threshold", () => {
    expect(kinds([read("a.rs"), read("b.rs")])).toEqual(["one", "one"]);
  });

  /* builds and tests are steps in their own right; swallowing them into a
     "ran 6 commands" row was the version of this that read worst */
  it("does not fold an execution into a looking run", () => {
    const out = kinds([
      read("a.rs"),
      read("b.rs"),
      read("c.rs"),
      cmd("cargo test"),
      read("d.rs"),
      read("e.rs"),
      read("f.rs"),
    ]);
    /* the run breaks around the build rather than swallowing it: a run
       either side, and the build standing on its own between them */
    expect(out).toEqual(["run", "one", "run"]);
  });

  /* git status is reading, git commit is not: a verb with subcommands is
     read to its second word */
  it("reads a subcommand before deciding what a command does", () => {
    expect(kinds([read("a.rs"), cmd("git status"), read("b.rs")])).toEqual(["run"]);
    expect(kinds([read("a.rs"), cmd("git commit -m x"), read("b.rs")])).toContain(
      "one",
    );
  });

  /* `sleep 20; cd x && ls` is three verbs and the first two are scaffolding */
  it("judges a compound command by all of its parts", () => {
    expect(kinds([read("a.rs"), cmd("cd src && ls"), read("b.rs")])).toEqual(["run"]);
    expect(kinds([read("a.rs"), cmd("cd src && cargo build"), read("b.rs")])).toContain(
      "one",
    );
  });

  it("never folds the live call", () => {
    const out = kinds([read("a.rs"), read("b.rs"), read("c.rs", "running")]);
    expect(out[out.length - 1]).toBe("one");
  });

  it("does not fold a run in which everything failed", () => {
    const out = kinds([
      cmd("grep a", "error", true),
      cmd("grep b", "error", true),
      cmd("grep c", "error", true),
    ]);
    expect(out).toEqual(["one", "one", "one"]);
  });

  it("names the first failure in a run that has one", () => {
    const boom = cmd("grep b", "error", true);
    expect(firstFailure([cmd("grep a"), boom, cmd("grep c", "error", true)])).toBe(
      boom,
    );
    expect(firstFailure([cmd("grep a")])).toBeNull();
  });

  it("folds repeats of one file from two, and keeps naming it", () => {
    const out = runs([read("src/api/export.rs"), read("src/api/export.rs")]);
    expect(out).toHaveLength(1);
    if (out[0]!.kind === "run") expect(out[0]!.label).toContain("export.rs");
  });

  it("keeps edits and reads in separate runs", () => {
    expect(
      labels([read("a.rs"), read("b.rs"), read("c.rs"), edit("d.rs")]),
    ).toHaveLength(2);
  });

  /* the transcript is a record: a fold may reorder nothing */
  it("preserves order and loses no call", () => {
    const input = [
      read("a.rs"),
      read("b.rs"),
      cmd("cargo test"),
      edit("c.rs"),
      read("d.rs"),
    ];
    const flat = runs(input).flatMap((r) => (r.kind === "one" ? [r.tool] : r.tools));
    expect(flat.map((t) => t.itemKey)).toEqual(input.map((t) => t.itemKey));
  });
});

describe("folding a worker", () => {
  /* a worker's steps are scattered through a turn; this is the one fold
     that deliberately moves a row past another */
  it("gathers one worker’s scattered steps into a single row", () => {
    const out = runs([
      worker("start", "Implementer", "s1"),
      read("a.rs"),
      worker("message", "Implementer", "s1"),
      read("b.rs"),
      worker("message", "Implementer", "s1"),
    ]);
    const workers = out.filter((r) => r.kind === "worker");
    expect(workers).toHaveLength(1);
    expect(workers[0]!.tools).toHaveLength(3);
  });

  it("keeps two workers apart", () => {
    const out = runs([
      worker("start", "Implementer", "s1"),
      worker("start", "Reviewer", "s2"),
      worker("message", "Implementer", "s1"),
      worker("message", "Reviewer", "s2"),
    ]);
    expect(out.filter((r) => r.kind === "worker")).toHaveLength(2);
  });

  it("keeps two sessions of the same agent apart", () => {
    const out = runs([
      worker("start", "Reviewer", "s1"),
      worker("start", "Reviewer", "s2"),
      worker("message", "Reviewer", "s1"),
    ]);
    const workers = out.filter((r) => r.kind === "worker");
    expect(workers).toHaveLength(1);
    expect(workers[0]!.tools).toHaveLength(2);
  });

  it("tells the worker’s story in the order it happened", () => {
    const story = workerStory([
      worker("start", "Implementer", "s1"),
      worker("message", "Implementer", "s1"),
      worker("message", "Implementer", "s1"),
    ]);
    expect(story).toBe("started · messaged ×2");
  });
});

describe("an empty or single transcript", () => {
  it("folds nothing", () => {
    expect(runs([])).toEqual([]);
    expect(kinds([read("a.rs")])).toEqual(["one"]);
  });
});
