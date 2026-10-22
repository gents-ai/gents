import { describe, expect, test } from "vitest";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
  NodeReadinessDecision,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { projectDeploymentOperationalState } from "@source-inc/gents-desktop-client";
import {
  projectChatShell as projectChatShellWithReadiness,
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
    // spawnSync trips the Vitest worker RPC timeout; use async spawn.
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
    case "turnInProgress":
      return {
        kind: "turnInProgress",
        nodeDid: nodeDid(contractCase.frontend_selected_node_did)!,
        sessionId: sessionId(contractCase.frontend_local_workflow_session)!,
        requestId: requestId(contractCase.frontend_local_workflow_request),
        turnState: contractCase.frontend_local_workflow_turn_state!,
      };
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
  test(
    "matches generated Lean ClientShell projection contracts",
    async () => {
      const contractCases = await loadLeanClientShellCases();
      expect(contractCases).toHaveLength(29);
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
});

describe("a session this app doesn't own", () => {
  const workerSummary = (unsendableReason: string | null) =>
    ({
      sessionId: "worker",
      nodeDid: "did:key:node",
      requesterDid: "did:key:node",
      turnState: null,
      latestRequestId: null,
      unsendableReason,
    }) as SessionSummary;
  const project = (summary: SessionSummary) =>
    projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:key:node",
      selectedSessionId: "worker",
      sending: false,
      session: null,
      selectedSessionSummary: summary,
      localWorkflow: { kind: "ready" },
    });

  test("blocks the composer with the bridge's reason", () => {
    const reason = "Read-only. Message the session that started it.";
    expect(project(workerSummary(reason)).nonEmptyContentSendStatus).toEqual({
      kind: "disabled",
      reason: "sessionNotOwned",
      hint: reason,
    });
  });

  test("doesn't block a session the app may send into", () => {
    expect(
      project(workerSummary(null)).nonEmptyContentSendStatus,
    ).not.toMatchObject({ reason: "sessionNotOwned" });
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
