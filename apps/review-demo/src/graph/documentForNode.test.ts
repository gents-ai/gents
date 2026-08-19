import { describe, expect, it } from "vitest";

import { documentForNode } from "./documentForNode.ts";
import type { GraphNode, ReviewSnapshot } from "./types.ts";

const snapshot: ReviewSnapshot = {
  jobs: [{ run_id: "run-1", focus: "look at triggers", repository_path: "." }],
  areas: [
    {
      run_id: "run-1",
      area_id: "run-1:lean",
      lens: "lean",
      instructions: "check proofs",
    },
  ],
  candidates: [],
  scans: [{ run_id: "run-1", area_id: "run-1:lean", summary: "one candidate" }],
  verdicts: [],
  summaries: [],
  findings: [],
  reports: [],
  requests: [],
  calls: [],
};

function node(partial: Partial<GraphNode> & Pick<GraphNode, "id" | "kind">): GraphNode {
  return {
    label: partial.id,
    state: "live",
    runId: "run-1",
    badges: [],
    ...partial,
  };
}

describe("documentForNode", () => {
  it("returns the ReviewJob, ReviewArea, and ScanResult for those nodes", () => {
    expect(documentForNode(node({ id: "job:run-1", kind: "job" }), snapshot)).toEqual({
      collection: "ReviewJob",
      fields: { run_id: "run-1", focus: "look at triggers", repository_path: "." },
    });
    expect(documentForNode(node({ id: "area:run-1:lean", kind: "area" }), snapshot)?.collection).toBe(
      "ReviewArea",
    );
    expect(documentForNode(node({ id: "scan:run-1:lean", kind: "scan" }), snapshot)).toMatchObject({
      collection: "ScanResult",
      fields: { summary: "one candidate" },
    });
  });

  it("does not return another run's document when area ids collide", () => {
    const twoRuns: ReviewSnapshot = {
      ...snapshot,
      areas: [
        { run_id: "run-2", area_id: "area-1", lens: "other" },
        { run_id: "run-1", area_id: "area-1", lens: "lean" },
      ],
      scans: [
        { run_id: "run-2", area_id: "area-1", summary: "wrong run" },
        { run_id: "run-1", area_id: "area-1", summary: "this run" },
      ],
    };
    expect(
      documentForNode(node({ id: "area:area-1", kind: "area" }), twoRuns)?.fields.lens,
    ).toBe("lean");
    expect(
      documentForNode(node({ id: "scan:area-1", kind: "scan" }), twoRuns)?.fields.summary,
    ).toBe("this run");
  });
});
