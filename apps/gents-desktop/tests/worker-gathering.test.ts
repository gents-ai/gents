import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";

import { gatherWorkers, workerStory } from "@/screens/worker-gathering";

let seq = 0;
const call = (presentation: RenderedToolCallView["presentation"]) =>
  ({
    itemKey: `t${++seq}`,
    toolName: "tool",
    statusKind: "success",
    presentation,
  }) as RenderedToolCallView;

const worker = (action: string, name: string, sessionId: string | null = null) =>
  call({ kind: "subagent", action, name, sessionId, description: null, output: null });

const read = (target: string) =>
  call({
    kind: "fileRead",
    operation: "read",
    target,
    returnedCount: null,
    totalCount: null,
    truncated: false,
    body: "",
    fallbackOutput: null,
  });

/* the distinct workers gathered, in the order their first call came */
const gathered = (tools: RenderedToolCallView[]) => [
  ...new Set(gatherWorkers(tools).values()),
];

describe("gathering a worker", () => {
  /* a worker's steps are scattered through a turn; this is the one place
     a row is deliberately moved past another */
  it("gathers one worker's scattered steps, in order", () => {
    const first = worker("start", "Implementer", "s1");
    const second = worker("message", "Implementer", "s1");
    const third = worker("message", "Implementer", "s1");
    const workers = gathered([first, read("a.rs"), second, read("b.rs"), third]);
    expect(workers).toHaveLength(1);
    expect(workers[0]!.tools).toEqual([first, second, third]);
  });

  it("keeps two workers apart", () => {
    const workers = gathered([
      worker("start", "Implementer", "s1"),
      worker("start", "Reviewer", "s2"),
      worker("message", "Implementer", "s1"),
      worker("message", "Reviewer", "s2"),
    ]);
    expect(workers.map((w) => w.key)).toEqual(["s1", "s2"]);
  });

  it("keeps two sessions of the same agent apart, and leaves a lone call alone", () => {
    const workers = gathered([
      worker("start", "Reviewer", "s1"),
      worker("start", "Reviewer", "s2"),
      worker("message", "Reviewer", "s1"),
    ]);
    expect(workers).toHaveLength(1);
    expect(workers[0]!.tools).toHaveLength(2);
  });

  it("gathers nothing from calls that reach no worker twice", () => {
    expect(gathered([])).toEqual([]);
    expect(gathered([read("a.rs"), worker("start", "Solo", "s9")])).toEqual([]);
  });

  it("tells the worker's story in the order it happened", () => {
    const story = workerStory([
      worker("start", "Implementer", "s1"),
      worker("message", "Implementer", "s1"),
      worker("message", "Implementer", "s1"),
    ]);
    expect(story).toBe("started · messaged ×2");
  });
});
