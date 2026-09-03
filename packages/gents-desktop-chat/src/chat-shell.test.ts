import { describe, expect, it, test } from "vitest";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

import type {
  BehaviorReadinessDecision,
  ConversationSummary,
  DeploymentView,
  DesktopSessionSnapshot,
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

const readyBehaviorReadiness = {
  kind: "ready",
  behaviorId: "general",
  behaviorLabel: "General",
} as const;

function operationalStateFor(
  decision: BehaviorReadinessDecision = readyBehaviorReadiness,
  routeReady = true,
) {
  const behaviorId = decision.behaviorId ?? "general";
  const readinessStatus =
    decision.kind === "unavailable"
      ? { state: "unavailable" as const, behaviorId, reason: decision.reason }
      : { state: "ready" as const, behaviorId };
  return projectDeploymentOperationalState({
    agentDid: "did:key:agent",
    source: "enrollment",
    dialSucceeded: true,
    chatSafe: routeReady,
    lastError: null,
    runtime: null,
    agentPrincipal: {
      agentDid: "did:key:agent",
      defaultBehaviorId: behaviorId,
    },
    behaviorReadiness: {
      source:
        decision.kind === "unknown"
          ? { state: "unknown", reason: decision.reason }
          : { state: "current" },
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      behaviors: [readinessStatus],
    },
    behaviors: [
      {
        behaviorId,
        displayName:
          decision.kind === "unknown" ? "General" : decision.behaviorLabel,
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
  frontend_selected_agent_did: number | null;
  frontend_selected_session_id: number | null;
  frontend_composer_non_empty: boolean;
  frontend_sending: boolean;
  frontend_session_present: boolean;
  frontend_session_id: number | null;
  frontend_session_latest_request_id: number | null;
  frontend_session_turn_state: TurnState | null;
  frontend_session_pending_request_id: number | null;
  frontend_conversation_present: boolean;
  frontend_conversation_session_id: number | null;
  frontend_conversation_latest_request_id: number | null;
  frontend_conversation_turn_state: TurnState | null;
  frontend_local_workflow_kind: string;
  frontend_local_workflow_session: number | null;
  frontend_local_workflow_request: number | null;
  frontend_local_workflow_turn_state: TurnState | null;
  frontend_expected_workflow_kind: string;
  frontend_expected_workflow_session: number | null;
  frontend_expected_workflow_request: number | null;
  frontend_expected_workflow_turn_state: TurnState | null;
  frontend_expected_workflow_reason: ChatBlockedReason | null;
  frontend_expected_send_status: "ready" | "disabled";
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

function conversation(
  overrides: Partial<ConversationSummary> = {},
): ConversationSummary {
  return {
    sessionId: "session-1",
    title: "conversation",
    previewText: "preview",
    status: "active",
    behaviorId: "default",
    latestRequestId: "req-1",
    createdAt: "2026-04-21T00:00:00Z",
    updatedAt: "2026-04-21T00:00:00Z",
    turnState: "completed",
    messageCount: 1,
    toolCallCount: 0,
    ...overrides,
  };
}

function session(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:test:amy",
    behaviorId: "default",
    title: "conversation",
    previewText: "preview",
    status: "active",
    turnState: "completed",
    latestRequestId: "req-1",
    retryEligibility: { eligible: false, denialReason: "notFailed" },
    latestResponse: null,
    activeResponseOverlay: null,
    pendingTurn: null,
    messages: [],
    toolCalls: [],
    toolResults: [],
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

function agentDid(id: number | null) {
  return id === null ? null : `did:test:agent-${id}`;
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
    agentDid: agentDid(contractCase.frontend_selected_agent_did),
    latestRequestId: requestId(contractCase.frontend_session_latest_request_id),
    turnState: contractCase.frontend_session_turn_state,
    pendingTurn: contractCase.frontend_session_pending_request_id
      ? {
          requestId: requestId(
            contractCase.frontend_session_pending_request_id,
          )!,
          content: "contract prompt",
          lifecycleState: "processing",
          createdAt: "2026-04-21T12:01:00Z",
        }
      : null,
  });
}

function conversationFromContract(contractCase: LeanClientShellCase) {
  if (!contractCase.frontend_conversation_present) {
    return null;
  }
  return conversation({
    sessionId:
      sessionId(contractCase.frontend_conversation_session_id) ??
      "session-missing",
    latestRequestId: requestId(
      contractCase.frontend_conversation_latest_request_id,
    ),
    turnState: contractCase.frontend_conversation_turn_state,
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
        agentDid:
          agentDid(contractCase.frontend_selected_agent_did) ??
          "did:test:agent-missing",
        sessionId: sessionId(contractCase.frontend_local_workflow_session),
      };
    case "awaitingObservation":
      return {
        kind: "awaitingObservation",
        agentDid:
          agentDid(contractCase.frontend_selected_agent_did) ??
          "did:test:agent-missing",
        sessionId:
          sessionId(contractCase.frontend_local_workflow_session) ??
          "session-missing",
        requestId:
          requestId(contractCase.frontend_local_workflow_request) ??
          "req-missing",
      };
    case "blocked":
      return {
        kind: "blocked",
        reason:
          contractCase.frontend_expected_workflow_reason ?? "clientOffline",
      };
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
        key !== "agentDid" && value !== undefined && value !== null,
    ),
  );
}

describe("projectChatShell", () => {
  it("blocks empty-draft and retry sends from a typed unavailable verdict", () => {
    const projection = projectChatShellWithReadiness({
      clientAvailable: true,
      selectedAgentDid: "did:key:agent",
      selectedSessionId: null,
      draft: "",
      sending: false,
      session: null,
      selectedConversation: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor({
        kind: "unavailable",
        behaviorId: "general",
        behaviorLabel: "General",
        reason: "backend_temporarily_unavailable",
      }),
    });

    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "behaviorUnavailable",
      hint: "Inference backend for “General” is temporarily unavailable",
    });
    expect(projection.nonEmptyContentSendStatus).toEqual(projection.sendStatus);
  });

  it("keeps route admission separate from runtime readiness", () => {
    const projection = projectChatShellWithReadiness({
      clientAvailable: true,
      selectedAgentDid: "did:key:agent",
      selectedSessionId: null,
      draft: "hello",
      sending: false,
      session: null,
      selectedConversation: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(readyBehaviorReadiness, false),
    });

    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "routeNotReady",
      hint: "The agent is connected, but its signed conversation route is not ready yet.",
    });
  });

  test(
    "matches generated Lean ClientShell projection contracts",
    async () => {
      const contractCases = await loadLeanClientShellCases();
      expect(contractCases).toHaveLength(17);

      for (const contractCase of contractCases) {
        const projection = projectChatShell({
          clientAvailable: contractCase.frontend_client_available,
          selectedAgentDid: agentDid(contractCase.frontend_selected_agent_did),
          selectedSessionId: sessionId(
            contractCase.frontend_selected_session_id,
          ),
          draft: contractCase.frontend_composer_non_empty ? "follow up" : "",
          sending: contractCase.frontend_sending,
          selectedConversation: conversationFromContract(contractCase),
          session: sessionFromContract(contractCase),
          localWorkflow: localWorkflowFromContract(contractCase),
        });

        expect(compactWorkflow(projection.workflow)).toEqual(
          expectedWorkflowFromContract(contractCase),
        );
        expect(projection.activeRequestId).toBe(
          requestId(contractCase.frontend_expected_active_request_id),
        );
        expect(projection.turnState).toBe(
          contractCase.frontend_expected_turn_state,
        );

        if (contractCase.frontend_expected_send_status === "ready") {
          expect(projection.sendStatus).toEqual({ kind: "ready" });
        } else {
          expect(projection.sendStatus.kind).toBe("disabled");
          if (projection.sendStatus.kind === "disabled") {
            expect(projection.sendStatus.reason).toBe(
              contractCase.frontend_expected_send_blocked_reason,
            );
          }
        }
      }
    },
    GENERATED_CONTRACT_TEST_TIMEOUT_MS,
  );

  test("blocks follow up while turn is streaming", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ turnState: "streaming" }),
      session: session({ turnState: "streaming", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "awaitingTurnTerminality",
      hint: "Turn still streaming",
    });
    expect(projection.activityStatus).toEqual({
      kind: "working",
      label: "Agent is working…",
      detail: "This turn must finish before another message can be sent.",
      animated: true,
    });
  });

  test("does not let an empty draft mask an active turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "",
      sending: false,
      selectedConversation: conversation({ turnState: "waitingForClaim" }),
      session: session({
        turnState: "waitingForClaim",
        latestRequestId: "req-1",
      }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "composerEmpty",
      hint: "Type a message to send",
    });
    expect(projection.nonEmptyContentSendStatus).toEqual({
      kind: "disabled",
      reason: "awaitingTurnTerminality",
      hint: "Waiting for the active turn to start",
    });
    expect(projection.activityStatus).toEqual({
      kind: "waiting",
      label: "Waiting for the agent…",
      detail: "Your message is queued until the enrolled agent claims it.",
      animated: true,
    });
  });

  test("uses tracked request before observed latest request catches up", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({
        latestRequestId: "req-old",
        turnState: "completed",
      }),
      session: session({
        latestRequestId: "req-new",
        turnState: "streaming",
        pendingTurn: {
          requestId: "req-new",
          content: "follow up",
          lifecycleState: "processing",
          createdAt: "2026-04-21T00:01:00Z",
        },
      }),
      localWorkflow: {
        kind: "awaitingObservation",
        agentDid: "did:test:amy",
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.activeRequestId).toBe("req-new");
    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.sendStatus.kind).toBe("disabled");
  });

  test("commits terminal projection before observing an automated follow-up", () => {
    const trackedWorkflow: ChatWorkflowState = {
      kind: "turnInProgress",
      agentDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-user",
      turnState: "streaming",
    };
    const terminalProjection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "",
      sending: false,
      selectedConversation: conversation({
        latestRequestId: "req-wake",
        turnState: "streaming",
      }),
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
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "",
      sending: false,
      selectedConversation: conversation({
        latestRequestId: "req-wake",
        turnState: "streaming",
      }),
      session: session({
        latestRequestId: "req-wake",
        turnState: "streaming",
      }),
      localWorkflow: reconciled,
    });

    expect(wakeProjection.activeRequestId).toBe("req-wake");
    expect(wakeProjection.workflow).toEqual({
      kind: "turnInProgress",
      agentDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-wake",
      turnState: "streaming",
    });
  });

  test("keeps awaiting observation until the matching request is observed", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({
        latestRequestId: "req-old",
        turnState: "completed",
      }),
      session: session({ latestRequestId: "req-old", turnState: "completed" }),
      localWorkflow: {
        kind: "awaitingObservation",
        agentDid: "did:test:amy",
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.workflow).toEqual({
      kind: "awaitingObservation",
      agentDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "req-new",
    });
    expect(projection.sendStatus).toEqual({
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
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-2",
      draft: "new session follow up",
      sending: false,
      selectedConversation: conversation({
        sessionId: "session-2",
        latestRequestId: "req-2",
        turnState: "completed",
      }),
      session: session({
        sessionId: "session-2",
        latestRequestId: "req-2",
        turnState: "completed",
      }),
      localWorkflow: {
        kind: "turnInProgress",
        agentDid: "did:test:amy",
        sessionId: "session-1",
        requestId: "req-1",
        turnState: "streaming",
      },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.activeRequestId).toBe("req-2");
    expect(projection.sendStatus).toEqual({ kind: "ready" });
  });

  test("blocks inconsistent observation when latest request is missing", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({
        latestRequestId: "req-missing",
        turnState: undefined,
      }),
      session: session({ latestRequestId: undefined, turnState: undefined }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({
      kind: "blocked",
      reason: "inconsistentTurnObservation",
      turnState: undefined,
    });
    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "inconsistentTurnObservation",
      hint: "Waiting for consistent turn observation",
    });
  });

  test("allows follow up after terminal turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ turnState: "completed" }),
      session: session({ turnState: "completed", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.sendStatus).toEqual({ kind: "ready" });
  });

  test("allows follow up after interrupted turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ turnState: "interrupted" }),
      session: session({ turnState: "interrupted", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.sendStatus).toEqual({ kind: "ready" });
  });

  test("allows follow up when conversation summary is missing but session snapshot is terminal", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:test:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: null,
      session: session({
        title: null,
        previewText: null,
        turnState: "completed",
        latestRequestId: "req-1",
      }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.sendStatus).toEqual({ kind: "ready" });
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
