import { describe, expect, test } from "vitest";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
  NodeReadinessDecision,
  RequestLifecycleState,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { projectDeploymentOperationalState } from "@source-inc/gents-desktop-client";
import {
  inferenceActivityStatus,
  projectChatShell as projectChatShellWithReadiness,
  reconcileProjectedWorkflow,
  requestProgressPresentation,
  type ChatBlockedReason,
  type ChatWorkflowState,
  type TurnState,
} from "./chat-shell.js";

const readyNodeReadiness = {
  kind: "ready",
  agentDid: "general",
  agentLabel: "General",
} as const;

function operationalStateFor(
  decision: NodeReadinessDecision = readyNodeReadiness,
  routeReady = true,
) {
  const agentDid = decision.agentDid ?? "general";
  const readinessStatus =
    decision.kind === "unavailable"
      ? { state: "unavailable" as const, agentDid, reason: decision.reason }
      : { state: "ready" as const, agentDid };
  return projectDeploymentOperationalState({
    nodeDid: "did:key:node",
    source: "enrollment",
    dialSucceeded: true,
    runtimeConnection: "connected",
    chatSafe: routeReady,
    chatRefusal: routeReady
      ? null
      : {
          cause: "no_endpoint",
          nextAction: "connect_runtime",
          detail: "connect this runtime with its authenticated endpoint",
        },
    lastError: null,
    runtime: null,
    node: {
      nodeDid: "did:key:node",
      defaultAgentDid: agentDid,
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
        agentDid,
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

type LeanFrontendInput = {
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
  frontend_session_pending_queue_request_ids: number[];
  frontend_session_folded_request_ids: number[];
  frontend_local_workflow_kind: string;
  frontend_local_workflow_session: number | null;
  frontend_local_workflow_request: number | null;
  frontend_local_workflow_turn_state: TurnState | null;
};

type LeanFrontendOutput = {
  frontend_expected_workflow_kind: string;
  frontend_expected_workflow_session: number | null;
  frontend_expected_workflow_request: number | null;
  frontend_expected_workflow_turn_state: TurnState | null;
  frontend_expected_workflow_reason: ChatBlockedReason | null;
  frontend_expected_send_status: string;
  frontend_expected_send_blocked_reason: ChatBlockedReason | null;
  frontend_expected_active_request_id: number | null;
  frontend_expected_turn_state: TurnState | null;
};

type LeanContractSnapshot = {
  families: Record<
    string,
    { cases: Array<{ input: unknown; expected: unknown }> }
  >;
};

type LeanFamilyCase<Input, Output> = { input: Input; expected: Output };

let leanContractSnapshot: LeanContractSnapshot | null = null;

function session(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    nodeDid: "did:test:amy",
    agentDid: "default",
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
      sessionCompactionCount: null,
      requestReductionHistoryLoaded: false,
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
  }

  return leanContractSnapshot;
}

async function leanFamilyCases<Input, Output>(
  path: string,
): Promise<Array<LeanFamilyCase<Input, Output>>> {
  const key = /^families::(\w+)::cases\(\)$/.exec(path)?.[1];
  if (!key) {
    throw new Error(`not a Lean family path: ${path}`);
  }
  const family = (await loadLeanContractSnapshot()).families[key];
  if (!family) {
    throw new Error(`Lean family ${key} was not emitted`);
  }
  return family.cases as Array<LeanFamilyCase<Input, Output>>;
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

function sessionFromContract(contractCase: LeanFrontendInput) {
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
    pendingQueue: contractCase.frontend_session_pending_queue_request_ids.map(
      (id) => ({
        requestDocId: `doc-${id}`,
        requestId: requestId(id)!,
        content: "queued prompt",
        editable: true,
        steerable: true,
        editGroup: null,
      }),
    ),
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
  contractCase: LeanFrontendInput,
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

function contractNumber(value: string | null | undefined, prefix: string) {
  if (value === null || value === undefined) {
    return null;
  }
  if (!value.startsWith(prefix)) {
    throw new Error(`${value} is not a contract ${prefix} id`);
  }
  return Number(value.slice(prefix.length));
}

function frontendObservation(
  projection: ReturnType<typeof projectChatShell>,
): LeanFrontendOutput {
  const workflow = projection.workflow as ChatWorkflowState & {
    sessionId?: string | null;
    requestId?: string | null;
    turnState?: TurnState | null;
    reason?: ChatBlockedReason | null;
  };
  const send = projection.nonEmptyContentSendStatus;
  return {
    frontend_expected_workflow_kind: workflow.kind,
    frontend_expected_workflow_session: contractNumber(
      workflow.sessionId,
      "session-",
    ),
    frontend_expected_workflow_request: contractNumber(
      workflow.requestId,
      "req-",
    ),
    frontend_expected_workflow_turn_state: workflow.turnState ?? null,
    frontend_expected_workflow_reason: workflow.reason ?? null,
    frontend_expected_send_status: send.kind,
    frontend_expected_send_blocked_reason:
      send.kind === "disabled" ? send.reason : null,
    frontend_expected_active_request_id: contractNumber(
      projection.activeRequestId,
      "req-",
    ),
    frontend_expected_turn_state: projection.turnState,
  };
}

describe("projectChatShell", () => {
  test(
    "matches generated Lean ClientShell projection contracts",
    async () => {
      const contractCases = await leanFamilyCases<
        LeanFrontendInput,
        LeanFrontendOutput
      >("families::client_shell_contracts_frontend::cases()");
      expect(contractCases.length).toBeGreaterThan(0);

      for (const { input, expected } of contractCases) {
        const projection = projectChatShell({
          clientAvailable: input.frontend_client_available,
          selectedNodeDid: nodeDid(input.frontend_selected_node_did),
          selectedSessionId: sessionId(input.frontend_selected_session_id),
          sending: input.frontend_sending,
          selectedSessionSummary: null,
          session: sessionFromContract(input),
          localWorkflow: localWorkflowFromContract(input),
        });

        expect(frontendObservation(projection), input.name).toEqual(expected);
        expect(
          Object.entries(projection.workflow).filter(
            ([key, value]) =>
              value !== undefined &&
              value !== null &&
              ![
                "kind",
                "nodeDid",
                "sessionId",
                "requestId",
                "turnState",
                "reason",
              ].includes(key),
          ),
          input.name,
        ).toEqual([]);
        if (projection.workflow.kind === "turnInProgress") {
          expect(projection.workflow.nodeDid).toBe(
            nodeDid(input.frontend_selected_node_did),
          );
        }
        const send = projection.nonEmptyContentSendStatus;
        if (send.kind === "ready") {
          expect(send).toEqual({ kind: "ready" });
        } else if (send.kind === "queue") {
          expect(send.turnState, input.name).toBe(projection.turnState);
        }
      }
    },
    GENERATED_CONTRACT_TEST_TIMEOUT_MS,
  );
});

test.each(["remove", "edit"])(
  "keeps the active turn target after pending-only message %s",
  (action) => {
    const common = {
      clientAvailable: true,
      selectedNodeDid: "did:test:amy",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
    };
    const awaiting = {
      kind: "awaitingObservation",
      nodeDid: "did:test:amy",
      sessionId: "session-1",
      requestId: "queued",
    } as const;
    const entry = {
      requestDocId: "queued-doc",
      requestId: "queued",
      content: "Queued message",
      editable: true,
      steerable: true,
      editGroup: null,
    };
    const observed = projectChatShell({
      ...common,
      localWorkflow: awaiting,
      session: session({ turnState: "running", pendingQueue: [entry] }),
    });
    expect(observed.workflow).toMatchObject({
      kind: "turnInProgress",
      requestId: "req-1",
    });
    const localWorkflow = reconcileProjectedWorkflow(
      awaiting,
      observed.workflow,
    );
    const next = projectChatShell({
      ...common,
      localWorkflow,
      session: session({
        turnState: "running",
        pendingQueue:
          action === "edit" ? [{ ...entry, requestId: "replacement" }] : [],
      }),
    });
    expect(next.workflow).toMatchObject({
      kind: "turnInProgress",
      requestId: "req-1",
    });
    expect(next.nonEmptyContentSendStatus.kind).toBe("queue");
  },
);

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

describe("a sent message held in the pending queue", () => {
  const sent: ChatWorkflowState = {
    kind: "awaitingObservation",
    nodeDid: "did:key:node",
    sessionId: "session-1",
    requestId: "req-sent",
  };
  const queuedAs = (requestId: string) => ({
    requestDocId: `doc-${requestId}`,
    requestId,
    content: "queued prompt",
    editable: true,
    steerable: false,
    editGroup: null,
  });
  const project = (overrides: Partial<DesktopSessionSnapshot>) =>
    projectChatShell({
      clientAvailable: true,
      selectedNodeDid: "did:key:node",
      selectedSessionId: "session-1",
      sending: false,
      selectedSessionSummary: null,
      session: session({ nodeDid: "did:key:node", ...overrides }),
      localWorkflow: sent,
    });

  test("is no longer awaited, so editing or removing it leaves the composer free", () => {
    const projection = project({
      turnState: "completed",
      pendingQueue: [queuedAs("req-sent")],
    });
    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.nonEmptyContentSendStatus.kind).toBe("ready");
  });

  test("waits behind the running turn rather than offering to stop itself", () => {
    const projection = project({
      turnState: "running",
      pendingQueue: [queuedAs("req-sent")],
    });
    expect(projection.workflow).toMatchObject({
      kind: "turnInProgress",
      requestId: "req-1",
    });
    expect(projection.activeRequestId).toBe("req-1");
  });
});

describe("why Send waits, in the person's terms", () => {
  const base = {
    clientAvailable: true,
    selectedNodeDid: "did:key:node",
    selectedSessionId: "session-1",
    sending: false,
    selectedSessionSummary: null,
  } as const;
  const sent: ChatWorkflowState = {
    kind: "awaitingObservation",
    nodeDid: "did:key:node",
    sessionId: "session-1",
    requestId: "req-sent",
  };

  test("while the send is still in flight", () => {
    expect(
      projectChatShell({
        ...base,
        sending: true,
        session: session({ nodeDid: "did:key:node" }),
        localWorkflow: {
          kind: "submittingRequest",
          nodeDid: "did:key:node",
          sessionId: "session-1",
        },
      }).nonEmptyContentSendStatus,
    ).toEqual({
      kind: "disabled",
      reason: "submittingRequest",
      hint: "Still sending your last message. Try again in a moment.",
    });
  });

  test("while the sent message has not reached the session yet", () => {
    expect(
      projectChatShell({
        ...base,
        session: session({ nodeDid: "did:key:node" }),
        localWorkflow: sent,
      }).nonEmptyContentSendStatus,
    ).toEqual({
      kind: "disabled",
      reason: "waitingForRequestObservation",
      hint: "Your last message is still on its way. Try again in a moment.",
    });
  });

  test("while the selected session has not loaded", () => {
    expect(
      projectChatShell({
        ...base,
        session: null,
        localWorkflow: { kind: "ready" },
      }).nonEmptyContentSendStatus,
    ).toEqual({
      kind: "disabled",
      reason: "sessionMissingFromSnapshot",
      hint: "This session is still loading. Try again in a moment.",
    });
  });

  test("while the latest message's state has not synced", () => {
    const expected = {
      kind: "disabled",
      reason: "inconsistentTurnObservation",
      hint: "Still syncing this session. Try again in a moment.",
    };
    expect(
      projectChatShell({
        ...base,
        session: session({ nodeDid: "did:key:node", turnState: null }),
        localWorkflow: { kind: "ready" },
      }).nonEmptyContentSendStatus,
    ).toEqual(expected);
    expect(
      projectChatShell({
        ...base,
        session: session({
          nodeDid: "did:key:node",
          turnState: null,
          latestRequestId: "req-sent",
        }),
        localWorkflow: sent,
      }).nonEmptyContentSendStatus,
    ).toEqual(expected);
  });

  test("behind a turn that has not started or is running", () => {
    const project = (turnState: TurnState) =>
      projectChatShell({
        ...base,
        session: session({ nodeDid: "did:key:node", turnState }),
        localWorkflow: { kind: "ready" },
      }).nonEmptyContentSendStatus;
    expect(project("waitingForClaim")).toMatchObject({
      kind: "queue",
      hint: "Queued behind the message waiting to start",
    });
    expect(project("running")).toMatchObject({
      kind: "queue",
      hint: "Queued behind the running turn",
    });
  });
});

describe("requestProgressPresentation", () => {
  test(
    "matches every generated Lean request lifecycle projection",
    async () => {
      const cases = await leanFamilyCases<
        { name: string; lifecycle_state: RequestLifecycleState },
        { label: string; animated: boolean }
      >("families::contracts_request_progress::cases()");
      expect(cases.length).toBeGreaterThan(0);
      for (const contractCase of cases) {
        expect(
          requestProgressPresentation(contractCase.input.lifecycle_state),
          contractCase.input.name,
        ).toEqual(contractCase.expected);
      }
    },
    GENERATED_CONTRACT_TEST_TIMEOUT_MS,
  );
});

test("inference activity has distinct shared presentation", () => {
  expect(inferenceActivityStatus("awaiting_provider")?.label).toBe(
    "Waiting for provider…",
  );
  expect(inferenceActivityStatus("working")?.label).toBe("Agent is working…");
  expect(inferenceActivityStatus("waiting")?.label).toBe("Waiting…");
  expect(inferenceActivityStatus("retrying")?.label).toBe("Retrying provider…");
  expect(inferenceActivityStatus("unavailable")?.label).toBe(
    "Activity unavailable",
  );
  expect(inferenceActivityStatus(null)).toBeNull();
});

test("running request presents its observed retry and clears on completion", () => {
  const common = {
    clientAvailable: true,
    selectedNodeDid: "did:test:amy",
    selectedSessionId: "session-1",
    sending: false,
    selectedSessionSummary: null,
    localWorkflow: { kind: "ready" } as const,
  };
  const running = projectChatShell({
    ...common,
    session: session({ turnState: "running", inferenceActivity: "retrying" }),
  });
  expect(running.activityStatus?.label).toBe("Retrying provider…");
  const completed = projectChatShell({
    ...common,
    session: session({ turnState: "completed", inferenceActivity: "retrying" }),
  });
  expect(completed.activityStatus).toBeNull();
});
