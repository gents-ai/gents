import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { projectReviewGraph } from "./projectReviewGraph.ts";
import type { ReviewSnapshot } from "./types.ts";

function emptySnapshot(): ReviewSnapshot {
  return {
    jobs: [],
    areas: [],
    candidates: [],
    scans: [],
    verdicts: [],
    summaries: [],
    findings: [],
    reports: [],
    requests: [],
    calls: [],
  };
}

describe("projectReviewGraph", () => {
  it("returns an expected skeleton when no documents exist", () => {
    const graph = projectReviewGraph(emptySnapshot());
    expect(graph.runId).toBeNull();
    expect(graph.nodes.map((node) => [node.kind, node.state])).toEqual([
      ["job", "expected"],
      ["area", "expected"],
      ["scan", "expected"],
      ["area", "expected"],
      ["scan", "expected"],
      ["verify", "expected"],
      ["triage", "expected"],
    ]);
  });

  it("marks recon live when only the newest job exists", () => {
    const graph = projectReviewGraph({
      ...emptySnapshot(),
      jobs: [{ run_id: "run-1", _docID: "job-1" }],
      requests: [
        {
          request_id: "req-recon",
          session_id: "sess-recon",
          caused_by_trigger_id: "review-recon",
          lifecycle_state: "processing",
        },
      ],
    });
    expect(graph.runId).toBe("run-1");
    const job = graph.nodes.find((node) => node.kind === "job");
    expect(job).toMatchObject({
      id: "job:run-1",
      state: "live",
      requestId: "req-recon",
      sessionId: "sess-recon",
      sourceDocId: "job-1",
    });
    expect(
      graph.nodes.filter((node) => node.kind === "area").every((node) => node.state === "expected"),
    ).toBe(true);
  });

  it("marks verify waiting-group when some but not all scan results exist", () => {
    const graph = projectReviewGraph({
      ...emptySnapshot(),
      jobs: [{ run_id: "run-1", _docID: "job-1" }],
      areas: [
        {
          run_id: "run-1",
          area_id: "run-1:lean",
          lens: "lean",
          expected_total: "2",
          _docID: "area-lean",
        },
        {
          run_id: "run-1",
          area_id: "run-1:auth",
          lens: "auth",
          expected_total: "2",
          _docID: "area-auth",
        },
      ],
      scans: [{ run_id: "run-1", area_id: "run-1:lean", _docID: "scan-lean" }],
      candidates: [
        { run_id: "run-1", finding_id: "run-1:lean:dup", area_id: "run-1:lean" },
      ],
      requests: [
        {
          request_id: "req-recon",
          caused_by_trigger_id: "review-recon",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-lean",
          caused_by_trigger_id: "review-scan",
          caused_by_source_doc_id: "area-lean",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-auth",
          caused_by_trigger_id: "review-scan",
          caused_by_source_doc_id: "area-auth",
          lifecycle_state: "processing",
        },
      ],
    });
    expect(graph.nodes.find((node) => node.id === "area:run-1:lean")).toMatchObject({
      label: "Area 2",
      detail: "lean",
      state: "done",
      requestId: "req-lean",
      badges: ["scanned", "1 candidate"],
    });
    expect(graph.nodes.find((node) => node.id === "area:run-1:auth")?.label).toBe("Area 1");
    expect(graph.nodes.find((node) => node.id === "scan:run-1:lean")).toMatchObject({
      label: "Scan 2",
      state: "done",
    });
    expect(graph.nodes.find((node) => node.id === "scan:run-1:auth")).toMatchObject({
      label: "Scan 1",
      state: "expected",
    });
    expect(graph.nodes.find((node) => node.id === "area:run-1:auth")?.state).toBe("live");
    expect(graph.nodes.find((node) => node.kind === "verify")).toMatchObject({
      id: "verify:run-1",
      state: "waiting-group",
    });
  });

  it("marks verify live, not done, while the group is closed but the verifier is still running", () => {
    const graph = projectReviewGraph({
      ...emptySnapshot(),
      jobs: [{ run_id: "run-1", _docID: "job-1" }],
      areas: [
        {
          run_id: "run-1",
          area_id: "run-1:lean",
          lens: "lean",
          expected_total: "1",
          _docID: "area-lean",
        },
      ],
      scans: [{ run_id: "run-1", area_id: "run-1:lean" }],
      requests: [
        {
          request_id: "req-recon",
          caused_by_trigger_id: "review-recon",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-scan",
          caused_by_trigger_id: "review-scan",
          caused_by_source_doc_id: "area-lean",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-verify",
          caused_by_trigger_id: "review-verify",
          lifecycle_state: "processing",
        },
      ],
    });
    expect(graph.nodes.find((node) => node.kind === "verify")).toMatchObject({
      state: "live",
      requestId: "req-verify",
    });
  });

  it("marks verify and triage done when the closed ledger exists", () => {
    const graph = projectReviewGraph({
      ...emptySnapshot(),
      jobs: [{ run_id: "run-1", _docID: "job-1" }],
      areas: [
        {
          run_id: "run-1",
          area_id: "run-1:lean",
          lens: "lean",
          expected_total: "1",
          _docID: "area-lean",
        },
      ],
      scans: [{ run_id: "run-1", area_id: "run-1:lean" }],
      candidates: [{ run_id: "run-1", finding_id: "f1", area_id: "run-1:lean" }],
      verdicts: [{ run_id: "run-1", finding_id: "f1" }],
      summaries: [{ run_id: "run-1", _docID: "summary-1" }],
      findings: [{ run_id: "run-1", finding_id: "f1" }],
      reports: [{ run_id: "run-1", _docID: "report-1" }],
      requests: [
        {
          request_id: "req-recon",
          caused_by_trigger_id: "review-recon",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-scan",
          caused_by_trigger_id: "review-scan",
          caused_by_source_doc_id: "area-lean",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-verify",
          caused_by_trigger_id: "review-verify",
          lifecycle_state: "completed",
          session_id: "sess-verify",
        },
        {
          request_id: "req-triage",
          caused_by_trigger_id: "review-triage",
          lifecycle_state: "completed",
        },
      ],
    });
    expect(graph.nodes.find((node) => node.kind === "verify")).toMatchObject({
      state: "done",
      requestId: "req-verify",
      badges: ["1 verdict", "1 finding"],
    });
    expect(graph.nodes.find((node) => node.kind === "verdict")).toMatchObject({
      id: "verdict:f1",
      label: "Verdict 1",
      state: "done",
      requestId: "req-verify",
    });
    expect(graph.nodes.find((node) => node.kind === "triage")).toMatchObject({
      state: "done",
      requestId: "req-triage",
      badges: ["report"],
    });
  });

  it("follows the newest request, not the most active finished run", () => {
    const graph = projectReviewGraph({
      ...emptySnapshot(),
      jobs: [{ run_id: "exp-live" }, { run_id: "review-later" }],
      areas: [
        {
          run_id: "exp-live",
          area_id: "exp-live:auth",
          lens: "auth",
          expected_total: "1",
          _docID: "area-auth",
        },
      ],
      requests: [
        {
          request_id: "req-live",
          caused_by_trigger_id: "review-recon",
          caused_by_correlation: "exp-live",
          created_at: "2026-08-18T10:00:00Z",
          lifecycle_state: "completed",
        },
        {
          request_id: "req-later",
          caused_by_trigger_id: "review-recon",
          caused_by_correlation: "review-later",
          created_at: "2026-08-18T18:30:00Z",
          lifecycle_state: "processing",
        },
      ],
    });
    expect(graph.runId).toBe("review-later");
  });

  it("watches a pinned run_id even when another job is newer", () => {
    const graph = projectReviewGraph(
      {
        ...emptySnapshot(),
        jobs: [
          { run_id: "exp-live", created_at: "2026-08-18T10:00:00Z" },
          { run_id: "review-later", created_at: "2026-08-18T18:30:00Z" },
        ],
        areas: [{ run_id: "review-later", area_id: "review-later:x", lens: "other" }],
        requests: [],
      },
      { pinnedRunId: "exp-live" },
    );
    expect(graph.runId).toBe("exp-live");
    expect(graph.nodes.filter((node) => node.kind === "area").every((node) => node.state === "expected")).toBe(
      true,
    );
  });

  it("watches the newest run_id when two jobs are present", () => {
    const graph = projectReviewGraph({
      ...emptySnapshot(),
      jobs: [
        { run_id: "run-old", created_at: "2026-08-18T10:00:00Z" },
        { run_id: "run-new", created_at: "2026-08-18T11:00:00Z" },
      ],
      areas: [
        { run_id: "run-old", area_id: "run-old:lean", lens: "old" },
        { run_id: "run-new", area_id: "run-new:auth", lens: "auth", expected_total: "1" },
      ],
      requests: [
        {
          request_id: "req-old",
          caused_by_trigger_id: "review-recon",
          caused_by_correlation: "run-old",
          created_at: "2026-08-18T10:00:00Z",
        },
        {
          request_id: "req-new",
          caused_by_trigger_id: "review-recon",
          caused_by_correlation: "run-new",
          created_at: "2026-08-18T11:00:00Z",
          lifecycle_state: "processing",
        },
      ],
    });
    expect(graph.runId).toBe("run-new");
    expect(graph.nodes.some((node) => node.detail === "old")).toBe(false);
    expect(graph.nodes.find((node) => node.kind === "area")?.label).toBe("Area 1");
    expect(graph.nodes.find((node) => node.kind === "job")?.requestId).toBe("req-new");
  });

  it("projects the checked-in mid-run fixture into a waiting group", () => {
    const snapshot = JSON.parse(
      readFileSync(
        join(dirname(fileURLToPath(import.meta.url)), "fixtures/mid-run.json"),
        "utf8",
      ),
    ) as ReviewSnapshot;
    const graph = projectReviewGraph(snapshot);
    expect(graph.nodes.find((node) => node.kind === "verify")?.state).toBe("waiting-group");
  });
});
