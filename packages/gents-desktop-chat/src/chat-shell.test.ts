import { describe, expect, it, test } from "vitest";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
  NodeReadinessDecision,
} from "@source-inc/gents-desktop-client";
import { projectDeploymentOperationalState } from "@source-inc/gents-desktop-client";
import {
  projectChatShell as projectChatShellWithReadiness,
  reconcileProjectedWorkflow,
  requestProgressPresentation,
  type ChatBlockedReason,
  type ChatWorkflowState,
  type TurnState,
} from "./chat-shell.js";

const readyNodeReadiness = {
  kind: "ready",
  agentId: "general",
  agentLabel: "General",
} as const;

function operationalStateFor(
  decision: NodeReadinessDecision = readyNodeReadiness,
  routeReady = true,
) {
  const agentId = decision.agentId ?? "general";
  const readinessStatus =
    decision.kind === "unavailable"
      ? { state: "unavailable" as const, agentId, reason: decision.reason }
      : { state: "ready" as const, agentId };
  return projectDeploymentOperationalState({
    nodeDid: "did:key:node",
    source: "enrollment",
    dialSucceeded: true,
    chatSafe: routeReady,
    lastError: null,
    runtime: null,
    node: {
      nodeDid: "did:key:node",
      defaultAgentId: agentId,
    },
    nodeReadiness: {
      source:
        decision.kind === "unknown"
          ? { state: "unknown", reason: decision.reason }
          : { state: "current" },
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      agents: [readinessStatus],
    },
    agents: [
      {
        agentId,
        displayName:
          decision.kind === "unknown" ? "General" : decision.agentLabel,
        enabled: true,
        isDefault: true,
      },
    ],
  } as DeploymentView);
}

function projectChatShell(
  input: Omit<
    Parameters<typeof projectChatShellWithReadiness>[0],
    "operationalState"
  >,
) {
  return projectChatShellWithReadiness({
    ...input,
    operationalState: operationalStateFor(),
  });
}

const CONTRACT_JSON_BEGIN = "---BEGIN GENTS LEAN CONTRACT JSON---";
const CONTRACT_JSON_END = "---END GENTS LEAN CONTRACT JSON---";
const GENERATED_CONTRACT_COMMAND_TIMEOUT_MS = 240000;
const GENERATED_CONTRACT_TEST_TIMEOUT_MS = 300000;

type LeanClientShellCase = {
  name: string;
  frontend_client_available: boolean;
  frontend_selected_node_did: number | null;
  frontend_selected_session_id: number | null;
  frontend_sending: boolean;
  frontend_session_present: boolean;
  frontend_session_id: number | null;
  frontend_session_latest_request_id: number | null;
  frontend_session_turn_state: TurnState | null;
  frontend_session_pending_request_id: number | null;
  frontend_session_queued_request_ids: number[];
  frontend_session_folded_request_ids: number[];
  frontend_local_workflow_kind: string;
  frontend_local_workflow_session: number | null;
  frontend_local_workflow_request: number | null;
  frontend_local_workflow_turn_state: TurnState | null;
  frontend_expected_workflow_kind: string;
  frontend_expected_workflow_session: number | null;
  frontend_expected_workflow_request: number | null;
  frontend_expected_workflow_turn_state: TurnState | null;
  frontend_expected_workflow_reason: ChatBlockedReason | null;
  frontend_expected_send_status: "ready" | "queue" | "disabled";
  frontend_expected_send_blocked_reason: ChatBlockedReason | null;
  frontend_expected_active_request_id: number | null;
  frontend_expected_turn_state: TurnState | null;
};

type LeanContractSnapshot = {
  frontend_client_shell_case_count: number;
  frontend_client_shell_cases: LeanClientShellCase[];
  request_progress_cases: Array<{
    name: string;
    lifecycleState: string;
    label: string;
    animated: boolean;
  }>;
};

let leanContractSnapshot: LeanContractSnapshot | null = null;

function session(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    nodeDid: "did:test:amy",
    agentId: "default",
    title: "conversation",
    previewText: "preview",
    status: "active",
    turnState: "completed",
    latestRequestId: "req-1",
    retryEligibility: { eligible: false, denialReason: "notFailed" },
    latestRequestOutcome: null,
    pendingTurn: null,
    queuedTurns: [],
    foldedInputs: [],
    goal: null,
    timelineItems: [],
    context: {
      estimatedDurableTokens: 0,
      estimatedConversationTokens: 0,
      contextWindow: 1,
      compactionThreshold: 0.8,
      compactionThresholdTokens: 1,
      compactionStrategy: "summary",
      durableMessageCount: 0,
      providerMessageCount: 0,
      totalCompactedMessages: 0,
      compactions: [],
      lastRequest: null,
    },
    ...overrides,
  };
}

async function loadLeanContractSnapshot(): Promise<LeanContractSnapshot> {
  if (!leanContractSnapshot) {
    const proofsDir = join(repoRoot(), "crates/gents/proofs");
    await runLeanCommand(proofsDir, ["build", "Proofs.Conformance.Contracts"]);
    const stdout = await runLeanCommand(proofsDir, [
      "env",
      "lean",
      "--run",
      "Proofs/Conformance/Contracts.lean",
    ]);
    const begin = uniqueMarkerPosition(stdout, CONTRACT_JSON_BEGIN);
    const end = uniqueMarkerPosition(stdout, CONTRACT_JSON_END);
    if (begin < 0 || end < 0 || begin >= end) {
      throw new Error(
        "Lean ClientShell contract JSON sentinel order is invalid",
      );
    }
    leanContractSnapshot = JSON.parse(
      stdout.slice(begin + CONTRACT_JSON_BEGIN.length, end).trim(),
    ) as LeanContractSnapshot;
    if (
      leanContractSnapshot.frontend_client_shell_case_count !==
      leanContractSnapshot.frontend_client_shell_cases.length
    ) {
      throw new Error(
        "Lean frontend ClientShell case count drifted from emitted cases",
      );
    }
  }

  return leanContractSnapshot;
}

async function loadLeanClientShellCases() {
  return (await loadLeanContractSnapshot()).frontend_client_shell_cases;
}

function runLeanCommand(proofsDir: string, args: string[]): Promise<string> {
  const command = `lake ${args.join(" ")}`;
  return new Promise((resolve, reject) => {
    // spawnSync blocked the Vitest worker for ~2 minutes, which trips the
    // default 60s RPC timeout (`Timeout calling "onTaskUpdate"`) even when
    // the Lean tests themselves pass.
    const child = spawn("lake", args, {
      cwd: proofsDir,
      timeout: GENERATED_CONTRACT_COMMAND_TIMEOUT_MS,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.on("error", (error) => {
      reject(
        new Error(`failed to run ${command} in ${proofsDir}: ${error.message}`),
      );
    });
    child.on("close", (status) => {
      if (status !== 0) {
        reject(
          new Error(
            `${command} failed in ${proofsDir}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
          ),
        );
        return;
      }
      resolve(stdout);
    });
  });
}

function uniqueMarkerPosition(stdout: string, marker: string) {
  const first = stdout.indexOf(marker);
  const last = stdout.lastIndexOf(marker);
  if (first < 0) {
    throw new Error(`Lean contract generator stdout did not contain ${marker}`);
  }
  if (first !== last) {
    throw new Error(
      `Lean contract generator stdout contained duplicate ${marker} sentinels`,
    );
  }
  return first;
}

function repoRoot() {
  let dir = dirname(fileURLToPath(import.meta.url));
  const root = parse(dir).root;
  while (dir !== root) {
    if (existsSync(join(dir, "crates/gents/proofs/lakefile.lean"))) {
      return dir;
    }
    dir = dirname(dir);
  }
  throw new Error("could not find repository root from chat-shell.test.ts");
}

function nodeDid(id: number | null) {
  return id === null ? null : `did:test:node-${id}`;
}

function sessionId(id: number | null) {
  return id === null ? null : `session-${id}`;
}

function requestId(id: number | null) {
  return id === null ? null : `req-${id}`;
}

function sessionFromContract(contractCase: LeanClientShellCase) {
  if (!contractCase.frontend_session_present) {
    return null;
  }
  return session({
    sessionId: sessionId(contractCase.frontend_session_id) ?? "session-missing",
    nodeDid: nodeDid(contractCase.frontend_selected_node_did),
    latestRequestId: requestId(contractCase.frontend_session_latest_request_id),
    turnState: contractCase.frontend_session_turn_state,
    pendingTurn: contractCase.frontend_session_pending_request_id
      ? {
          requestId: requestId(
            contractCase.frontend_session_pending_request_id,
          )!,
          content: "contract prompt",
          lifecycleState: "processing",
          foldedIntoRequestId: null,
          origin: null,
          createdAt: "2026-04-21T12:01:00Z",
        }
      : null,
    queuedTurns: contractCase.frontend_session_queued_request_ids.map((id) => ({
      requestId: requestId(id)!,
      content: "queued prompt",
      selectedSkillIds: [],
      lifecycleState: "pending",
      foldedIntoRequestId: null,
      origin: null,
      createdAt: "2026-04-21T12:01:30Z",
    })),
    foldedInputs: contractCase.frontend_session_folded_request_ids.map(
      (id) => ({
        requestId: requestId(id)!,
        foldedIntoRequestId: requestId(
          contractCase.frontend_session_latest_request_id,
        )!,
      }),
    ),
  });
}

function localWorkflowFromContract(
  contractCase: LeanClientShellCase,
): ChatWorkflowState {
  switch (contractCase.frontend_local_workflow_kind) {
    case "ready":
      return { kind: "ready" };
    case "submittingRequest":
      return {
        kind: "submittingRequest",
        nodeDid:
          nodeDid(contractCase.frontend_selected_node_did) ??
          "did:test:node-missing",
        sessionId: sessionId(contractCase.frontend_local_workflow_session),
      };
    case "awaitingObservation":
      return {
        kind: "awaitingObservation",
        nodeDid:
          nodeDid(contractCase.frontend_selected_node_did) ??
          "did:test:node-missing",
        sessionId:
          sessionId(contractCase.frontend_local_workflow_session) ??
          "session-missing",
        requestId:
          requestId(contractCase.frontend_local_workflow_request) ??
          "req-missing",
      };
    case "blocked":
      // The contract has no blocked-workflow input reason; expected output
      // cannot supply it. Reject unsupported cases instead of seeding answers.
      throw new Error(
        `Lean case ${contractCase.name} needs a blocked-workflow input reason`,
      );
    default:
      throw new Error(
        `unsupported Lean frontend workflow ${contractCase.frontend_local_workflow_kind}`,
      );
  }
}

function expectedWorkflowFromContract(contractCase: LeanClientShellCase) {
  switch (contractCase.frontend_expected_workflow_kind) {
    case "ready":
      return { kind: "ready" };
    case "submittingRequest":
      return {
        kind: "submittingRequest",
        sessionId: sessionId(contractCase.frontend_expected_workflow_session),
      };
    case "awaitingObservation":
      return {
        kind: "awaitingObservation",
        sessionId: sessionId(contractCase.frontend_expected_workflow_session),
        requestId: requestId(contractCase.frontend_expected_workflow_request),
      };
    case "turnInProgress":
      return {
        kind: "turnInProgress",
        sessionId: sessionId(contractCase.frontend_expected_workflow_session),
        requestId: requestId(contractCase.frontend_expected_workflow_request),
        turnState: contractCase.frontend_expected_workflow_turn_state,
      };
    case "blocked":
      return {
        kind: "blocked",
        reason: contractCase.frontend_expected_workflow_reason,
      };
    default:
      throw new Error(
        `unsupported Lean expected workflow ${contractCase.frontend_expected_workflow_kind}`,
      );
  }
}

function compactWorkflow(workflow: ChatWorkflowState) {
  return Object.fromEntries(
    Object.entries(workflow).filter(
      ([key, value]) =>
        key !== "nodeDid" && value !== undefined && value !== null,
    ),
  );
}

describe("projectChatShell", () => {
  it("blocks sends from a typed unavailable verdict", () => {
    const projection = projectChatShellWithReadiness({
      clientAvailable: true,
      selectedNodeDid: "did:key:node",
      selectedSessionId: null,
      sending: false,
      session: null,
      selectedSessionSummary: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor({
        kind: "unavailable",
        agentId: "general",
        agentLabel: "General",
        reason: "backend_temporarily_unavailable",
      }),
    });

    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "disabled",
      reason: "agentUnavailable",
      hint: "Inference backend for “General” is temporarily unavailable",
    });
  });

  it("keeps route admission separate from runtime readiness", () => {
    const operationalState = operationalStateFor(readyNodeReadiness, false);
    const projection = projectChatShellWithReadiness({
      clientAvailable: true,
      selectedNodeDid: "did:key:node",
      selectedSessionId: null,
      sending: false,
      session: null,
      selectedSessionSummary: null,
      localWorkflow: { kind: "ready" },
      operationalState,
    });

    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "disabled",
      reason: "routeNotReady",
      hint: "The pairing request was sent and is waiting for the agent to accept it.",
    });
  });

  test(
    "matches generated Lean ClientShell projection contracts",
    async () => {
      const contractCases = await loadLeanClientShellCases();
      expect(contractCases).toHaveLength(27);
      expect(
        contractCases.some(
          (contractCase) =>
            contractCase.frontend_session_folded_request_ids.length > 0,
        ),
      ).toBe(true);
      expect(
        contractCases.some(
          (contractCase) =>
            contractCase.frontend_session_queued_request_ids.length > 0,
        ),
      ).toBe(true);

      for (const contractCase of contractCases) {
        const projection = projectChatShell({
          clientAvailable: contractCase.frontend_client_available,
          selectedNodeDid: nodeDid(contractCase.frontend_selected_node_did),
          selectedSessionId: sessionId(
            contractCase.frontend_selected_session_id,
          ),
          sending: contractCase.frontend_sending,
          selectedSessionSummary: null,
          session: sessionFromContract(contractCase),
          localWorkflow: localWorkflowFromContract(contractCase),
        });

        expect(compactWorkflow(projection.workflow), contractCase.name).toEqual(
          expectedWorkflowFromContract(contractCase),
        );
        // The compacted shape omits the node stamped by the projection.
        if (projection.workflow.kind === "turnInProgress") {
          expect(projection.workflow.nodeDid).toBe(
            nodeDid(contractCase.frontend_selected_node_did),
          );
        }
        expect(projection.activeRequestId).toBe(
          requestId(contractCase.frontend_expected_active_request_id),
        );
        expect(projection.turnState).toBe(
          contractCase.frontend_expected_turn_state,
        );

        if (contractCase.frontend_expected_send_status === "ready") {
          expect(projection.nonEmptyContentSendStatus).toEqual({
            kind: "ready",
          });
        } else if (contractCase.frontend_expected_send_status === "queue") {
          expect(
            projection.nonEmptyContentSendStatus.kind,
            contractCase.name,
          ).toBe("queue");
          if (projection.nonEmptyContentSendStatus.kind === "queue") {
            expect(projection.nonEmptyContentSendStatus.turnState).toBe(
              contractCase.frontend_expected_turn_state,
            );
          }
        } else {
          expect(projection.nonEmptyContentSendStatus.kind).toBe("disabled");
          if (projection.nonEmptyContentSendStatus.kind === "disabled") {
            expect(projection.nonEmptyContentSendStatus.reason).toBe(
              contractCase.frontend_expected_send_blocked_reason,
            );
          }
        }
      }
    },
    GENERATED_CONTRACT_TEST_TIMEOUT_MS,
  );

  test("queues a follow up while the turn is streaming", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({ turnState: "running", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "queue",
      turnState: "running",
      hint: "Queued behind the running turn",
    });
    expect(projection.activityStatus).toEqual({
      kind: "working",
      label: "Agent is working…",
      detail: "Messages you send now wait until this turn finishes.",
      animated: true,
    });
  });

  test("an active turn blocks sends and shows why", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        turnState: "waitingForClaim",
        latestRequestId: "req-1",
      }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "queue",
      turnState: "waitingForClaim",
      hint: "Queued behind the message waiting to start",
    });
    expect(projection.activityStatus).toEqual({
      kind: "waiting",
      label: "Waiting for the node…",
      detail:
        "The node has not started this request yet. Messages you send now wait behind it.",
      animated: true,
    });
  });

  test("a submission observed as queued tracks the running turn it waits behind", () => {
    const awaiting: ChatWorkflowState = {
      kind: "awaitingObservation",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-queued",
    };
    const observed = session({
      latestRequestId: "req-running",
      turnState: "running",
      queuedTurns: [
        {
          requestId: "req-queued",
          content: "and also this",
          selectedSkillIds: [],
          lifecycleState: "pending",
          foldedIntoRequestId: null,
          origin: null,
          createdAt: "2026-04-21T00:01:00Z",
        },
      ],
    });
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "one more",
      sending: false,
      selectedSessionSummary: null,
      session: observed,
      localWorkflow: awaiting,
    });

    expect(projection.workflow).toEqual({
      kind: "turnInProgress",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-running",
      turnState: "running",
    });
    expect(projection.activeRequestId).toBe("req-running");
    expect(projection.nonEmptyContentSendStatus.kind).toBe("queue");
    expect(reconcileProjectedWorkflow(awaiting, projection.workflow)).toEqual(
      projection.workflow,
    );
  });

  test("a submission folded before any queued snapshot retires once its turn ends", () => {
    const awaiting: ChatWorkflowState = {
      kind: "awaitingObservation",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-folded",
    };
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        latestRequestId: "req-head",
        turnState: "completed",
        foldedInputs: [
          { requestId: "req-folded", foldedIntoRequestId: "req-head" },
        ],
      }),
      localWorkflow: awaiting,
    });
    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(reconcileProjectedWorkflow(awaiting, projection.workflow)).toEqual({
      kind: "ready",
    });
  });

  test("uses tracked request before observed latest request catches up", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        latestRequestId: "req-new",
        turnState: "running",
        pendingTurn: {
          requestId: "req-new",
          content: "follow up",
          lifecycleState: "processing",
          foldedIntoRequestId: null,
          origin: null,
          createdAt: "2026-04-21T00:01:00Z",
        },
      }),
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: "did:test:amy",
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.activeRequestId).toBe("req-new");
    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.nonEmptyContentSendStatus.kind).toBe("queue");
  });

  test("commits terminal projection before observing an automated follow-up", () => {
    const trackedWorkflow: ChatWorkflowState = {
      kind: "turnInProgress",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-user",
      turnState: "running",
    };
    const terminalProjection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        latestRequestId: "req-user",
        turnState: "completed",
      }),
      localWorkflow: trackedWorkflow,
    });

    expect(terminalProjection.workflow).toEqual({ kind: "ready" });
    const reconciled = reconcileProjectedWorkflow(
      trackedWorkflow,
      terminalProjection.workflow,
    );
    expect(reconciled).toEqual({ kind: "ready" });

    const wakeProjection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        latestRequestId: "req-wake",
        turnState: "running",
      }),
      localWorkflow: reconciled,
    });

    expect(wakeProjection.activeRequestId).toBe("req-wake");
    expect(wakeProjection.workflow).toEqual({
      kind: "turnInProgress",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-wake",
      turnState: "running",
    });
  });

  test("keeps awaiting observation until the matching request is observed", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({ latestRequestId: "req-old", turnState: "completed" }),
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: "did:test:amy",
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.workflow).toEqual({
      kind: "awaitingObservation",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-new",
    });
    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "disabled",
      reason: "waitingForRequestObservation",
      hint: "Waiting for request observation",
    });
    expect(projection.activityStatus).toEqual({
      kind: "syncing",
      label: "Syncing message…",
      detail:
        "Your request was created; waiting for it to appear in the shared conversation.",
      animated: true,
    });
  });

  test("ignores stale tracked workflow after user switches sessions", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-2",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        sessionId: "session-2",
        latestRequestId: "req-2",
        turnState: "completed",
      }),
      localWorkflow: {
        kind: "turnInProgress",
        nodeDid: "did:test:amy",
        sessionId: "session-1",
        requestId: "req-1",
        turnState: "running",
      },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.activeRequestId).toBe("req-2");
    expect(projection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });
  });

  test("blocks inconsistent observation when latest request is missing", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        latestRequestId: "req-missing",
        turnState: undefined,
      }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({
      kind: "blocked",
      reason: "inconsistentTurnObservation",
      turnState: undefined,
    });
    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "disabled",
      reason: "inconsistentTurnObservation",
      hint: "Waiting for consistent turn observation",
    });
  });

  test("allows follow up after terminal turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({ turnState: "completed", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });
  });

  test("allows follow up after interrupted turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({ turnState: "interrupted", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });
  });

  test("allows follow up with an untitled terminal session", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({
        title: null,
        previewText: null,
        turnState: "completed",
        latestRequestId: "req-1",
      }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });
  });
});

describe("requestProgressPresentation", () => {
  test(
    "matches every generated Lean request lifecycle projection",
    async () => {
      const cases = (await loadLeanContractSnapshot()).request_progress_cases;
      expect(cases).toHaveLength(9);
      for (const contractCase of cases) {
        expect(
          requestProgressPresentation(contractCase.lifecycleState),
        ).toEqual({
          label: contractCase.label,
          animated: contractCase.animated,
        });
      }
    },
    GENERATED_CONTRACT_TEST_TIMEOUT_MS,
  );
});
