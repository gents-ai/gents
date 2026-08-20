import type { AgentRequestRow } from "../graph/types.ts";
import type {
  DefenseGraph,
  DefenseNode,
  DefenseNodeState,
  DefenseSnapshot,
} from "./types.ts";

const FAILED = new Set(["failed", "dead", "interrupted", "superseded"]);
const DONE = new Set(["completed", "complete"]);

export type DefenseProjectOptions = {
  pinnedRunId?: string | null;
};

function stateFor(
  request: AgentRequestRow | undefined,
  documentExists: boolean,
): DefenseNodeState {
  const lifecycle = request?.lifecycle_state ?? request?.status ?? "";
  if (lifecycle === "inputRequired") {
    return "input-required";
  }
  const normalized = lifecycle.toLowerCase();
  if (FAILED.has(normalized)) {
    return "failed";
  }
  if (DONE.has(normalized) || (documentExists && !request)) {
    return "done";
  }
  if (request || documentExists) {
    return "live";
  }
  return "expected";
}

function coordinatorState(
  request: AgentRequestRow | undefined,
  documentExists: boolean,
): DefenseNodeState {
  const state = stateFor(request, documentExists);
  return state === "done" && !documentExists ? "live" : state;
}

function requestFor(
  requests: AgentRequestRow[],
  triggerId: string,
  sourceDocId?: string,
): AgentRequestRow | undefined {
  return requests
    .filter((request) => {
      if (request.caused_by_trigger_id !== triggerId) {
        return false;
      }
      return !sourceDocId || request.caused_by_source_doc_id === sourceDocId;
    })
    .sort((left, right) =>
      (left.created_at ?? "").localeCompare(right.created_at ?? ""),
    )
    .at(-1);
}

function verifierRequestFor(
  requests: AgentRequestRow[],
  parentRequestId: string | undefined,
  findingId: string,
  assignmentDocId?: string,
): AgentRequestRow | undefined {
  return requests
    .filter((request) => {
      if (request.behavior_id !== "defend-verifier") {
        return false;
      }
      if (
        assignmentDocId &&
        request.caused_by_trigger_id === "defend-verifier" &&
        request.caused_by_source_doc_id === assignmentDocId
      ) {
        return true;
      }
      if (
        parentRequestId &&
        request.caused_by_parent_request_id !== parentRequestId
      ) {
        return false;
      }
      const content = request.content ?? "";
      return (
        content.includes(`finding_id: ${findingId}`) ||
        content.includes(`finding_id=${findingId}`) ||
        content.includes(`\"finding_id\":\"${findingId}\"`) ||
        content.includes(`\`${findingId}\``)
      );
    })
    .sort((left, right) =>
      (left.created_at ?? "").localeCompare(right.created_at ?? ""),
    )
    .at(-1);
}

function verifierActivity(
  request: AgentRequestRow | undefined,
  verdictExists: boolean,
): string {
  if (verdictExists) {
    return "verified";
  }
  if (!request) {
    return "queued";
  }
  const lifecycle = (request.lifecycle_state ?? request.status ?? "").toLowerCase();
  if (FAILED.has(lifecycle)) {
    return "failed";
  }
  if (lifecycle === "processing" || lifecycle === "running") {
    return "running";
  }
  if (lifecycle === "inputrequired" || lifecycle === "input-required") {
    return "input required";
  }
  if (DONE.has(lifecycle)) {
    return "completed · verdict pending";
  }
  return "queued";
}

function node(
  partial: Omit<DefenseNode, "badges"> & { badges?: string[] },
): DefenseNode {
  return { badges: [], ...partial };
}

function selectJob(snapshot: DefenseSnapshot, pinnedRunId?: string | null) {
  if (pinnedRunId) {
    const pinned = snapshot.jobs.find((job) => job.run_id === pinnedRunId);
    if (pinned) {
      return pinned;
    }
  }
  return snapshot.jobs
    .slice()
    .sort((left, right) => {
      const latest = (runId: string) =>
        snapshot.requests
          .filter((request) => request.caused_by_correlation === runId)
          .map((request) => request.created_at ?? "")
          .sort()
          .at(-1) ?? "";
      return latest(left.run_id).localeCompare(latest(right.run_id));
    })
    .at(-1);
}

export function projectDefenseGraph(
  snapshot: DefenseSnapshot,
  options: DefenseProjectOptions = {},
): DefenseGraph {
  const job = selectJob(snapshot, options.pinnedRunId);
  if (!job) {
    return skeleton(null, 8);
  }

  const runId = job.run_id;
  const requests = snapshot.requests.filter(
    (request) =>
      !request.caused_by_correlation || request.caused_by_correlation === runId,
  );
  const threat = snapshot.threats.find((row) => row.run_id === runId);
  const areas = snapshot.areas
    .filter((row) => row.run_id === runId)
    .slice()
    .sort((left, right) => left.area_id.localeCompare(right.area_id));
  const scans = snapshot.scans.filter((row) => row.run_id === runId);
  const candidates = snapshot.candidates.filter((row) => row.run_id === runId);
  const verificationAssignments = snapshot.verificationAssignments.filter(
    (row) => row.run_id === runId,
  );
  const verificationCompletions = snapshot.verificationCompletions.filter(
    (row) => row.run_id === runId,
  );
  const verdicts = snapshot.verdicts.filter((row) => row.run_id === runId);
  const findings = snapshot.findings.filter((row) => row.run_id === runId);
  const triage = snapshot.triage.find((row) => row.run_id === runId);
  const assignments = snapshot.assignments
    .filter((row) => row.run_id === runId)
    .slice()
    .sort((left, right) =>
      left.assignment_id.localeCompare(right.assignment_id),
    );
  const patches = snapshot.patches.filter((row) => row.run_id === runId);
  const reviews = snapshot.reviews.filter((row) => row.run_id === runId);
  const report = snapshot.reports.find((row) => row.run_id === runId);

  const threatRequest = requestFor(requests, "defend-threat-model", job._docID);
  const planRequest = requestFor(requests, "defend-plan", threat?._docID);
  const triageRequest = requestFor(requests, "defend-triage");
  const verificationPlanRequest = requestFor(
    requests,
    "defend-verification-plan",
  );
  const reportRequest = requestFor(requests, "defend-report");
  const expectedAreas = positiveInt(
    areas[0]?.expected_total ?? job.area_min,
    4,
  );

  const nodes: DefenseNode[] = [
    node({
      id: `job:${runId}`,
      kind: "job",
      label: "Defense job",
      detail: job.focus,
      state: "done",
      runId,
      sourceDocId: job._docID,
    }),
    node({
      id: `threat:${runId}`,
      kind: "threat",
      label: "Threat model",
      detail: threat?.system_context,
      state: stateFor(threatRequest, Boolean(threat)),
      runId,
      requestId: threatRequest?.request_id,
      sessionId: threatRequest?.session_id ?? undefined,
      sourceDocId: threat?._docID,
      badges: threat ? ["written"] : [],
    }),
    node({
      id: `plan:${runId}`,
      kind: "plan",
      label: "Plan areas",
      state: stateFor(planRequest, areas.length === expectedAreas),
      runId,
      requestId: planRequest?.request_id,
      sessionId: planRequest?.session_id ?? undefined,
      badges: [`${areas.length}/${expectedAreas} areas`],
    }),
  ];

  if (areas.length === 0) {
    for (let index = 0; index < expectedAreas; index += 1) {
      const key = `pending-${index}`;
      nodes.push(
        node({
          id: `area:${key}`,
          kind: "area",
          label: `Area ${index + 1}`,
          state: "expected",
          runId,
        }),
      );
    }
  } else {
    for (const [index, area] of areas.entries()) {
      const scan = scans.find((row) => row.area_id === area.area_id);
      const request = requestFor(requests, "defend-scan", area._docID);
      const findingCount = candidates.filter(
        (row) => row.area_id === area.area_id,
      ).length;
      nodes.push(
        node({
          id: `area:${area.area_id}`,
          kind: "area",
          label: `Area ${index + 1}`,
          detail: area.focus ?? area.area_id,
          state: stateFor(request, Boolean(scan)),
          runId,
          requestId: request?.request_id,
          sessionId: request?.session_id ?? undefined,
          sourceDocId: area._docID,
          badges: [
            ...(area.threat_ids ? [area.threat_ids] : []),
            ...(scan ? ["complete"] : []),
            ...(findingCount > 0
              ? [`${findingCount} candidate${findingCount === 1 ? "" : "s"}`]
              : []),
          ],
        }),
      );
      if (scan) {
        nodes.push(
          node({
            id: `scan:${area.area_id}`,
            kind: "scan",
            label: `Scan ${index + 1}`,
            detail: area.focus ?? area.area_id,
            state: "done",
            runId,
            sourceDocId: scan._docID,
            badges: scan.finding_count
              ? [`${scan.finding_count} findings`]
              : [],
          }),
        );
      }
    }
    for (let index = areas.length; index < expectedAreas; index += 1) {
      const key = `pending-${index}`;
      nodes.push(
        node({
          id: `area:${key}`,
          kind: "area",
          label: `Area ${index + 1}`,
          state: "expected",
          runId,
        }),
      );
    }
  }

  const scansClosed = scans.length === expectedAreas;
  const graphNativeVerification = Boolean(
    verificationPlanRequest ||
      verificationAssignments.length > 0 ||
      requests.some((request) => request.caused_by_trigger_id === "defend-verifier"),
  );
  const sortedCandidates = candidates
    .slice()
    .sort((left, right) => left.finding_id.localeCompare(right.finding_id));
  const candidateWork = sortedCandidates.map((candidate) => {
    const assignment = verificationAssignments.find(
      (row) => row.finding_id === candidate.finding_id,
    );
    const verdict = verdicts.find(
      (row) => row.finding_id === candidate.finding_id,
    );
    const verifierRequest = verifierRequestFor(
      requests,
      triageRequest?.request_id,
      candidate.finding_id,
      assignment?._docID,
    );
    return { assignment, candidate, verdict, verifierRequest };
  });
  const isolatedVerifierCount = candidateWork.filter(
    ({ verifierRequest }) => verifierRequest,
  ).length;
  const runningVerifierCount = candidateWork.filter(
    ({ verifierRequest, verdict }) =>
      verifierActivity(verifierRequest, Boolean(verdict)) === "running",
  ).length;
  const queuedVerifierCount = candidateWork.filter(
    ({ verifierRequest, verdict }) =>
      verifierActivity(verifierRequest, Boolean(verdict)) === "queued",
  ).length;
  const isolatedVerifierTopology = Boolean(
    graphNativeVerification ||
      triageRequest?.content?.includes("candidate-verifier") ||
      triageRequest?.content?.includes("spawn_subagent"),
  );
  const legacySerialTriage = Boolean(
    scansClosed &&
      candidates.length > 0 &&
      triageRequest &&
      !isolatedVerifierTopology &&
      isolatedVerifierCount === 0 &&
      !triage,
  );
  if (graphNativeVerification) {
    nodes.push(
      node({
        id: `verification-plan:${runId}`,
        kind: "verification-plan",
        label: "Verification work set",
        state: stateFor(
          verificationPlanRequest,
          verificationAssignments.length > 0,
        ),
        runId,
        requestId: verificationPlanRequest?.request_id,
        sessionId: verificationPlanRequest?.session_id ?? undefined,
        badges: [
          `${verificationAssignments.length} assignments`,
          "document fan-out",
        ],
      }),
    );
  }
  const expectedVerdicts = positiveInt(
    verificationAssignments[0]?.expected_total,
    candidates.length || 1,
  );
  const verificationClosed =
    graphNativeVerification &&
    verificationAssignments.length > 0 &&
    verificationCompletions.length === expectedVerdicts;
  nodes.push(
    node({
      id: `triage:${runId}`,
      kind: "triage",
      label: "Adversarial triage",
      state: graphNativeVerification
        ? verificationClosed
          ? stateFor(triageRequest, Boolean(triage))
          : "waiting-group"
        : scansClosed
          ? coordinatorState(triageRequest, Boolean(triage))
          : "waiting-group",
      runId,
      requestId: triageRequest?.request_id,
      sessionId: triageRequest?.session_id ?? undefined,
      sourceDocId: triage?._docID,
      badges: [
        ...(graphNativeVerification
          ? [
              `${verificationCompletions.length}/${expectedVerdicts} complete`,
              `${verdicts.length}/${candidates.length} verdicts`,
            ]
          : [`${scans.length}/${expectedAreas} scans`]),
        ...(!scansClosed && candidates.length > 0
          ? [`${candidates.length} candidates queued`]
          : []),
        ...(legacySerialTriage
          ? ["serial triage", "active candidate untracked"]
          : []),
        ...(scansClosed &&
        candidates.length > 0 &&
        !legacySerialTriage &&
        !graphNativeVerification
          ? [`${verdicts.length}/${candidates.length} verdicts`]
          : []),
        ...(isolatedVerifierCount > 0
          ? [`${runningVerifierCount} running`, `${queuedVerifierCount} queued`]
          : []),
        ...(findings.length > 0 ? [`${findings.length} confirmed`] : []),
      ],
    }),
  );

  if (scansClosed || graphNativeVerification) {
    for (const [index, { assignment, candidate, verdict, verifierRequest }] of
      candidateWork.entries()) {
      const activity = legacySerialTriage
        ? "activity untracked"
        : verifierActivity(verifierRequest, Boolean(verdict));
      nodes.push(
        node({
          id: `candidate:${candidate.finding_id}`,
          kind: "candidate",
          label: `Candidate ${index + 1}`,
          detail: candidate.title ?? candidate.finding_id,
          state: "done",
          runId,
          sourceDocId: candidate._docID,
          badges: [
            ...(candidate.claimed_severity
              ? [candidate.claimed_severity]
              : []),
            ...(candidate.area_id ? [candidate.area_id] : []),
            activity,
          ],
        }),
      );
      if (assignment) {
        nodes.push(
          node({
            id: `verification-assignment:${candidate.finding_id}`,
            kind: "verification-assignment",
            label: `Assignment ${index + 1}`,
            detail: assignment.assignment_id,
            state: "done",
            runId,
            sourceDocId: assignment._docID,
            badges: assignment.status ? [assignment.status] : [],
          }),
        );
      }
      if (verifierRequest) {
        nodes.push(
          node({
            id: `verifier:${candidate.finding_id}`,
            kind: "verifier",
            label: `Verifier ${index + 1}`,
            detail: candidate.finding_id,
            state: stateFor(verifierRequest, Boolean(verdict)),
            runId,
            requestId: verifierRequest.request_id,
            sessionId: verifierRequest.session_id ?? undefined,
            badges: [verifierActivity(verifierRequest, Boolean(verdict))],
          }),
        );
      }
      if (verdict) {
        nodes.push(
          node({
            id: `verdict:${candidate.finding_id}`,
            kind: "verdict",
            label: `Verdict ${index + 1}`,
            detail: verdict.title ?? candidate.finding_id,
            state: "done",
            runId,
            sourceDocId: verdict._docID,
            badges: [
              ...(verdict.verdict ? [verdict.verdict] : []),
              ...(verdict.severity ? [verdict.severity] : []),
            ],
          }),
        );
      }
    }
  }

  if (assignments.length === 0) {
    nodes.push(
      node({
        id: "assignment:pending",
        kind: "assignment",
        label: "Patch set",
        state: "expected",
        runId,
      }),
      node({
        id: "patch:pending",
        kind: "patch",
        label: "Draft",
        state: "expected",
        runId,
      }),
      node({
        id: "review:pending",
        kind: "review",
        label: "Review",
        state: "expected",
        runId,
      }),
    );
  } else {
    for (const [index, assignment] of assignments.entries()) {
      const patch = patches.find(
        (row) => row.patch_id === assignment.assignment_id,
      );
      const review = reviews.find(
        (row) => row.patch_id === assignment.assignment_id,
      );
      const patchRequest = requestFor(
        requests,
        "defend-patch",
        assignment._docID,
      );
      const reviewRequest = requestFor(
        requests,
        "defend-patch-review",
        patch?._docID,
      );
      nodes.push(
        node({
          id: `assignment:${assignment.assignment_id}`,
          kind: "assignment",
          label: `Finding ${index + 1}`,
          detail: assignment.finding_id,
          state: "done",
          runId,
          sourceDocId: assignment._docID,
          badges: assignment.status ? [assignment.status] : [],
        }),
        node({
          id: `patch:${assignment.assignment_id}`,
          kind: "patch",
          label: `Patch ${index + 1}`,
          detail: assignment.finding_id,
          state: stateFor(patchRequest, Boolean(patch)),
          runId,
          requestId: patchRequest?.request_id,
          sessionId: patchRequest?.session_id ?? undefined,
          sourceDocId: patch?._docID,
          badges: patch?.status ? [patch.status] : [],
        }),
        node({
          id: `review:${assignment.assignment_id}`,
          kind: "review",
          label: `Review ${index + 1}`,
          detail: assignment.finding_id,
          state: stateFor(reviewRequest, Boolean(review)),
          runId,
          requestId: reviewRequest?.request_id,
          sessionId: reviewRequest?.session_id ?? undefined,
          sourceDocId: review?._docID,
          badges: review?.verdict ? [review.verdict] : [],
        }),
      );
    }
  }

  const reviewsClosed =
    assignments.length > 0 && reviews.length === assignments.length;
  nodes.push(
    node({
      id: `report:${runId}`,
      kind: "report",
      label: "Defense report",
      state: reviewsClosed
        ? stateFor(reportRequest, Boolean(report))
        : "waiting-group",
      runId,
      requestId: reportRequest?.request_id,
      sessionId: reportRequest?.session_id ?? undefined,
      sourceDocId: report?._docID,
      badges: report
        ? [
            `${report.confirmed_count ?? "0"} confirmed`,
            `${report.accepted_patch_count ?? "0"} accepted`,
          ]
        : [],
    }),
  );

  return { runId, nodes };
}

function skeleton(runId: string | null, areaCount: number): DefenseGraph {
  const id = runId ?? "pending";
  const nodes: DefenseNode[] = [
    node({
      id: `job:${id}`,
      kind: "job",
      label: "Defense job",
      state: "expected",
      runId: id,
    }),
    node({
      id: `threat:${id}`,
      kind: "threat",
      label: "Threat model",
      state: "expected",
      runId: id,
    }),
    node({
      id: `plan:${id}`,
      kind: "plan",
      label: "Plan areas",
      state: "expected",
      runId: id,
    }),
  ];
  for (let index = 0; index < areaCount; index += 1) {
    nodes.push(
      node({
        id: `area:pending-${index}`,
        kind: "area",
        label: `Area ${index + 1}`,
        state: "expected",
        runId: id,
      }),
    );
  }
  nodes.push(
    node({
      id: `triage:${id}`,
      kind: "triage",
      label: "Adversarial triage",
      state: "expected",
      runId: id,
    }),
    node({
      id: "assignment:pending",
      kind: "assignment",
      label: "Patch set",
      state: "expected",
      runId: id,
    }),
    node({
      id: "patch:pending",
      kind: "patch",
      label: "Draft",
      state: "expected",
      runId: id,
    }),
    node({
      id: "review:pending",
      kind: "review",
      label: "Review",
      state: "expected",
      runId: id,
    }),
    node({
      id: `report:${id}`,
      kind: "report",
      label: "Defense report",
      state: "expected",
      runId: id,
    }),
  );
  return { runId, nodes };
}

function positiveInt(value: string | undefined, fallback: number): number {
  const parsed = Number.parseInt(value ?? "", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}
