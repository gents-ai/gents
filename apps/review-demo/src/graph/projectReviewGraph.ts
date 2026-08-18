import type {
  AgentRequestRow,
  GraphEdge,
  GraphNode,
  NodeState,
  ReviewGraph,
  ReviewSnapshot,
} from "./types.ts";

const FAILED = new Set([
  "failed",
  "error",
  "timedout",
  "cancelled",
  "interrupted",
  "superseded",
  "dead",
]);

const DONE = new Set(["completed"]);

function requestState(request: AgentRequestRow | undefined, exists: boolean): NodeState {
  if (!exists) {
    return "expected";
  }
  const lifecycle = request?.lifecycle_state?.toLowerCase() ?? "";
  if (FAILED.has(lifecycle)) {
    return "failed";
  }
  if (DONE.has(lifecycle)) {
    return "done";
  }
  return "live";
}

export type ProjectOptions = {
  pinnedRunId?: string | null;
};

function jobRecency(snapshot: ReviewSnapshot, runId: string): string {
  const job = snapshot.jobs.find((row) => row.run_id === runId);
  if (job?.created_at) {
    return job.created_at;
  }
  const recon = snapshot.requests.find(
    (request) =>
      request.caused_by_trigger_id === "review-recon" &&
      (request.caused_by_correlation === runId || !request.caused_by_correlation),
  );
  return recon?.created_at ?? "";
}

function jobActivity(snapshot: ReviewSnapshot, runId: string): number {
  const count = (rows: { run_id: string }[]) => rows.filter((row) => row.run_id === runId).length;
  const requests = snapshot.requests.filter((request) => request.caused_by_correlation === runId)
    .length;
  return (
    count(snapshot.areas) * 10 +
    count(snapshot.scans) * 10 +
    count(snapshot.candidates) +
    count(snapshot.verdicts) +
    count(snapshot.summaries) * 5 +
    count(snapshot.reports) * 5 +
    requests
  );
}

function selectJob(snapshot: ReviewSnapshot, pinnedRunId?: string | null) {
  if (snapshot.jobs.length === 0) {
    return null;
  }
  if (pinnedRunId) {
    const pinned = snapshot.jobs.find((job) => job.run_id === pinnedRunId);
    if (pinned) {
      return pinned;
    }
  }
  return [...snapshot.jobs].sort((left, right) => {
    const activity = jobActivity(snapshot, left.run_id) - jobActivity(snapshot, right.run_id);
    if (activity !== 0) {
      return activity;
    }
    const leftAt = jobRecency(snapshot, left.run_id);
    const rightAt = jobRecency(snapshot, right.run_id);
    if (leftAt !== rightAt) {
      return leftAt < rightAt ? -1 : 1;
    }
    return left.run_id < right.run_id ? -1 : 1;
  }).at(-1)!;
}

function findRequest(
  requests: AgentRequestRow[],
  triggerId: string,
  sourceDocId?: string,
): AgentRequestRow | undefined {
  return requests.find((request) => {
    if (request.caused_by_trigger_id !== triggerId) {
      return false;
    }
    if (sourceDocId && request.caused_by_source_doc_id) {
      return request.caused_by_source_doc_id === sourceDocId;
    }
    return true;
  });
}

function node(
  partial: Omit<GraphNode, "badges"> & { badges?: string[] },
): GraphNode {
  return { badges: [], ...partial };
}

export function projectReviewGraph(
  snapshot: ReviewSnapshot,
  options: ProjectOptions = {},
): ReviewGraph {
  const job = selectJob(snapshot, options.pinnedRunId);
  if (!job) {
    return {
      runId: null,
      nodes: [
        node({ id: "job:pending", kind: "job", label: "ReviewJob", state: "expected", runId: "" }),
        node({ id: "area:pending-0", kind: "area", label: "Area 1", state: "expected", runId: "" }),
        node({ id: "scan:pending-0", kind: "scan", label: "Scan 1", state: "expected", runId: "" }),
        node({ id: "area:pending-1", kind: "area", label: "Area 2", state: "expected", runId: "" }),
        node({ id: "scan:pending-1", kind: "scan", label: "Scan 2", state: "expected", runId: "" }),
        node({
          id: "verify:pending",
          kind: "verify",
          label: "Verify",
          state: "expected",
          runId: "",
        }),
        node({
          id: "triage:pending",
          kind: "triage",
          label: "Triage",
          state: "expected",
          runId: "",
        }),
      ],
      edges: [
        { from: "job:pending", to: "area:pending-0" },
        { from: "area:pending-0", to: "scan:pending-0" },
        { from: "scan:pending-0", to: "verify:pending" },
        { from: "job:pending", to: "area:pending-1" },
        { from: "area:pending-1", to: "scan:pending-1" },
        { from: "scan:pending-1", to: "verify:pending" },
        { from: "verify:pending", to: "triage:pending" },
      ],
    };
  }

  const runId = job.run_id;
  const areas = snapshot.areas
    .filter((row) => row.run_id === runId)
    .slice()
    .sort((left, right) => left.area_id.localeCompare(right.area_id));
  const scans = snapshot.scans.filter((row) => row.run_id === runId);
  const candidates = snapshot.candidates.filter((row) => row.run_id === runId);
  const verdicts = snapshot.verdicts.filter((row) => row.run_id === runId);
  const findings = snapshot.findings.filter((row) => row.run_id === runId);
  const summary = snapshot.summaries.find((row) => row.run_id === runId);
  const report = snapshot.reports.find((row) => row.run_id === runId);
  const requests = snapshot.requests.filter((request) => {
    if (!request.caused_by_correlation) {
      return true;
    }
    return request.caused_by_correlation === runId;
  });

  const recon = findRequest(requests, "review-recon", job._docID);
  const verifyReq = findRequest(requests, "review-verify");
  const triageReq = findRequest(requests, "review-triage");

  const expectedTotal = Number.parseInt(areas[0]?.expected_total ?? "", 10);
  const areaSlots =
    areas.length > 0
      ? areas
      : Number.isFinite(expectedTotal) && expectedTotal > 0
        ? []
        : [];

  const nodes: GraphNode[] = [
    node({
      id: `job:${runId}`,
      kind: "job",
      label: "ReviewJob",
      state: requestState(recon, true),
      runId,
      requestId: recon?.request_id,
      sessionId: recon?.session_id ?? undefined,
      sourceDocId: job._docID,
    }),
  ];
  const edges: GraphEdge[] = [];

  const areaNodes =
    areaSlots.length > 0
      ? areaSlots.map((area, index) => {
          const request = findRequest(requests, "review-scan", area._docID);
          const scan = scans.find((row) => row.area_id === area.area_id);
          const findingCount = candidates.filter((row) => row.area_id === area.area_id).length;
          const areaId = `area:${area.area_id}`;
          const scanId = `scan:${area.area_id}`;
          const areaState = requestState(request, true);
          const badges: string[] = [];
          if (scan) {
            badges.push("scanned");
          }
          if (findingCount > 0) {
            badges.push(`${findingCount} candidate${findingCount === 1 ? "" : "s"}`);
          }
          nodes.push(
            node({
              id: areaId,
              kind: "area",
              label: `Area ${index + 1}`,
              detail: area.lens || area.area_id,
              state: areaState,
              runId,
              requestId: request?.request_id,
              sessionId: request?.session_id ?? undefined,
              sourceDocId: area._docID,
              badges,
            }),
          );
          edges.push({ from: `job:${runId}`, to: areaId });
          nodes.push(
            node({
              id: scanId,
              kind: "scan",
              label: `Scan ${index + 1}`,
              detail: area.lens || area.area_id,
              state: scan ? requestState(request, true) : "expected",
              runId,
              requestId: request?.request_id,
              sessionId: request?.session_id ?? undefined,
              sourceDocId: scan?._docID,
            }),
          );
          edges.push({ from: areaId, to: scanId });
          edges.push({ from: scanId, to: `verify:${runId}` });
          return areaId;
        })
      : (() => {
          const placeholders = ["pending-0", "pending-1"];
          for (const [index, key] of placeholders.entries()) {
            const areaId = `area:${key}`;
            const scanId = `scan:${key}`;
            nodes.push(
              node({
                id: areaId,
                kind: "area",
                label: `Area ${index + 1}`,
                state: "expected",
                runId,
              }),
            );
            nodes.push(
              node({
                id: scanId,
                kind: "scan",
                label: `Scan ${index + 1}`,
                state: "expected",
                runId,
              }),
            );
            edges.push({ from: `job:${runId}`, to: areaId });
            edges.push({ from: areaId, to: scanId });
            edges.push({ from: scanId, to: `verify:${runId}` });
          }
          return placeholders.map((key) => `area:${key}`);
        })();

  void areaNodes;

  let verifyState: NodeState = "expected";
  if (summary) {
    verifyState = requestState(verifyReq, true);
  } else if (scans.length > 0) {
    const expected = Number.parseInt(areas[0]?.expected_total ?? String(areas.length), 10);
    verifyState =
      Number.isFinite(expected) && scans.length < expected ? "waiting-group" : requestState(verifyReq, Boolean(verifyReq));
  } else if (verifyReq) {
    verifyState = requestState(verifyReq, true);
  }

  nodes.push(
    node({
      id: `verify:${runId}`,
      kind: "verify",
      label: "Verify",
      state: verifyState,
      runId,
      requestId: verifyReq?.request_id,
      sessionId: verifyReq?.session_id ?? undefined,
      sourceDocId: summary?._docID,
      badges: [
        ...(verdicts.length > 0
          ? [`${verdicts.length} verdict${verdicts.length === 1 ? "" : "s"}`]
          : []),
        ...(findings.length > 0
          ? [`${findings.length} finding${findings.length === 1 ? "" : "s"}`]
          : []),
      ],
    }),
  );

  const sortedVerdicts = verdicts
    .slice()
    .sort((left, right) => left.finding_id.localeCompare(right.finding_id));
  for (const [index, verdict] of sortedVerdicts.entries()) {
    const verdictId = `verdict:${verdict.finding_id}`;
    nodes.push(
      node({
        id: verdictId,
        kind: "verdict",
        label: `Verdict ${index + 1}`,
        detail: verdict.finding_id,
        state: requestState(verifyReq, Boolean(summary || verifyReq)),
        runId,
        requestId: verifyReq?.request_id,
        sessionId: verifyReq?.session_id ?? undefined,
      }),
    );
    edges.push({ from: `verify:${runId}`, to: verdictId });
    edges.push({ from: verdictId, to: `triage:${runId}` });
  }
  if (sortedVerdicts.length === 0) {
    edges.push({ from: `verify:${runId}`, to: `triage:${runId}` });
  }

  const triageState = report || triageReq ? requestState(triageReq, Boolean(report || triageReq)) : "expected";
  const triageBadges: string[] = [];
  if (report) {
    triageBadges.push("report");
  }
  nodes.push(
    node({
      id: `triage:${runId}`,
      kind: "triage",
      label: "Triage",
      state: triageState,
      runId,
      requestId: triageReq?.request_id,
      sessionId: triageReq?.session_id ?? undefined,
      sourceDocId: report?._docID,
      badges: triageBadges,
    }),
  );

  return { runId, nodes, edges };
}
