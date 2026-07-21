import { describe, expect, it, test } from "vitest";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

import type { ConversationSummary, DesktopSessionSnapshot } from "./types";
import {
  projectChatShell,
  type ChatBlockedReason,
  type ChatWorkflowState,
  type TurnState,
} from "./chat-shell";

const CONTRACT_JSON_BEGIN = "---BEGIN DEFRA LEAN CONTRACT JSON---";
const CONTRACT_JSON_END = "---END DEFRA LEAN CONTRACT JSON---";
// The generated contract test shells out to Lake before reading Lean JSON.
const GENERATED_CONTRACT_TEST_TIMEOUT_MS = 30000;

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

type LeanLiveOverlayCase = {
  name: string;
  responseStatus: "streaming" | "complete" | "error";
  materialized: boolean;
  precedingToolCalls: number;
  turnTerminal: boolean;
  turnLabel: string;
  hasContent: boolean;
  hasReasoning: boolean;
  expectOverlay: boolean;
};

type LeanContractSnapshot = {
  frontend_client_shell_case_count: number;
  frontend_client_shell_cases: LeanClientShellCase[];
  live_overlay_cases: LeanLiveOverlayCase[];
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
    agentDid: "did:defra:amy",
    behaviorId: "default",
    title: "conversation",
    previewText: "preview",
    status: "active",
    turnState: "completed",
    latestRequestId: "req-1",
    latestResponse: null,
    activeResponseOverlay: null,
    pendingTurn: null,
    messages: [],
    toolCalls: [],
    toolResults: [],
    ...overrides,
  };
}

function loadLeanContractSnapshot(): LeanContractSnapshot {
  if (!leanContractSnapshot) {
    const proofsDir = join(repoRoot(), "crates/gents/proofs");
    runLeanCommand(proofsDir, ["build", "Proofs.Conformance.Contracts"]);
    const stdout = runLeanCommand(proofsDir, [
      "env",
      "lean",
      "--run",
      "Proofs/Conformance/Contracts.lean",
    ]);
    const begin = uniqueMarkerPosition(stdout, CONTRACT_JSON_BEGIN);
    const end = uniqueMarkerPosition(stdout, CONTRACT_JSON_END);
    if (begin < 0 || end < 0 || begin >= end) {
      throw new Error("Lean ClientShell contract JSON sentinel order is invalid");
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

function loadLeanClientShellCases() {
  return loadLeanContractSnapshot().frontend_client_shell_cases;
}

function loadLeanLiveOverlayCases() {
  return loadLeanContractSnapshot().live_overlay_cases;
}

function runLeanCommand(proofsDir: string, args: string[]) {
  const command = `lake ${args.join(" ")}`;
  const result = spawnSync("lake", args, {
    cwd: proofsDir,
    encoding: "utf8",
  });

  if (result.error) {
    throw new Error(
      `failed to run ${command} in ${proofsDir}: ${result.error.message}`,
    );
  }
  if (result.status !== 0) {
    throw new Error(
      `${command} failed in ${proofsDir}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
    );
  }

  return result.stdout.toString();
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
  return id === null ? null : `did:defra:agent-${id}`;
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
          requestId: requestId(contractCase.frontend_session_pending_request_id)!,
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
      sessionId(contractCase.frontend_conversation_session_id) ?? "session-missing",
    latestRequestId: requestId(contractCase.frontend_conversation_latest_request_id),
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
          "did:defra:agent-missing",
        sessionId: sessionId(contractCase.frontend_local_workflow_session),
      };
    case "awaitingObservation":
      return {
        kind: "awaitingObservation",
        sessionId:
          sessionId(contractCase.frontend_local_workflow_session) ?? "session-missing",
        requestId:
          requestId(contractCase.frontend_local_workflow_request) ?? "req-missing",
      };
    case "blocked":
      return {
        kind: "blocked",
        reason: contractCase.frontend_expected_workflow_reason ?? "clientOffline",
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
  // The Lean frontend contract models rendered workflow fields; TypeScript keeps
  // agentDid on submittingRequest for the API callback path.
  return Object.fromEntries(
    Object.entries(workflow).filter(
      ([key, value]) => key !== "agentDid" && value !== undefined && value !== null,
    ),
  );
}

describe("projectChatShell", () => {
  test(
    "matches generated Lean ClientShell projection contracts",
    () => {
      const contractCases = loadLeanClientShellCases();
      expect(contractCases).toHaveLength(15);

      for (const contractCase of contractCases) {
        const projection = projectChatShell({
          clientAvailable: contractCase.frontend_client_available,
          selectedAgentDid: agentDid(contractCase.frontend_selected_agent_did),
          selectedSessionId: sessionId(contractCase.frontend_selected_session_id),
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
        expect(projection.turnState).toBe(contractCase.frontend_expected_turn_state);

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
      selectedAgentDid: "did:defra:amy",
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
  });

  test("uses tracked request before observed latest request catches up", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
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
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.activeRequestId).toBe("req-new");
    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.sendStatus.kind).toBe("disabled");
  });

  test("keeps awaiting observation until the matching request is observed", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
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
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.workflow).toEqual({
      kind: "awaitingObservation",
      sessionId: "session-1",
      requestId: "req-new",
    });
    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "waitingForRequestObservation",
      hint: "Waiting for request observation",
    });
  });

  test("ignores stale tracked workflow after user switches sessions", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
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
      selectedAgentDid: "did:defra:amy",
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
      selectedAgentDid: "did:defra:amy",
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
      selectedAgentDid: "did:defra:amy",
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
      selectedAgentDid: "did:defra:amy",
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

/**
 * Mirror of the Lean `projectActiveOverlay` decision (see
 * `Proofs/ClientShell/Projection.lean`) and of the Rust
 * `should_show_overlay` helper in
 * `crates/defra-agent/tests/live_overlay_conformance.rs`. Kept inline so the
 * TypeScript live-delta render decision can fail loudly if either side drifts
 * from the contract.
 */
function shouldShowLiveOverlay(c: LeanLiveOverlayCase): boolean {
  if (c.materialized) {
    return false;
  }
  if (c.responseStatus === "complete" || c.responseStatus === "error") {
    return false;
  }
  // The Lean predicate hides the overlay for any non-renderable turn label,
  // including terminal labels (completed/failed/superseded/interrupted) and
  // the unmodelled labels we may receive over the wire.
  if (c.turnLabel !== "streaming" && c.turnLabel !== "waitingForClaim") {
    return false;
  }
  return c.hasContent || c.hasReasoning;
}

describe("LiveOverlay conformance (issue #64)", () => {
  const cases = loadLeanLiveOverlayCases();

  it("Lean LiveOverlay case table is non-empty", () => {
    expect(cases.length).toBeGreaterThan(0);
  });

  for (const raw of cases) {
    it(`case ${raw.name} matches Lean expected decision`, () => {
      expect(shouldShowLiveOverlay(raw)).toBe(raw.expectOverlay);
      if (raw.turnTerminal) {
        // Sanity: terminal turns must hide the overlay regardless of content,
        // mirroring the Rust live_overlay_conformance integration test.
        expect(raw.expectOverlay).toBe(false);
      }
      // Reference precedingToolCalls so a Lean column drop fails the snapshot.
      expect(typeof raw.precedingToolCalls).toBe("number");
    });
  }
});
