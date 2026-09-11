import type { BackendHealth } from "@source-inc/gents-desktop-client";
import { deriveDisplayState } from "@source-inc/gents-desktop-operations";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type {
  DesktopClientUpdatedHandler,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";
import type {
  CascadeCancelPreview,
  CodexLoginResult,
  DesktopClientSnapshot,
  DesktopOperationsSnapshot,
  DesktopSessionSnapshot,
  DeploymentView,
  EnrollmentRequestView,
  GrokLoginResult,
  InitSummary,
  InferenceBackendView,
  InterruptRequestResult,
  MCPServiceHealthView,
  McpServiceProbeResult,
  SubagentTreeView,
  SyncHealthView,
  TaskRunResult,
  ToolServiceTestResult,
} from "@source-inc/gents-desktop-client";

import type {
  AgentPrincipal,
  AgentBehavior,
  AgentContext,
  BehaviorView,
  ResponseView,
  SessionSummary,
  SessionProvenance,
  TriggerView,
} from "@source-inc/gents-desktop-client";
import type { SessionContextView } from "@source-inc/gents-desktop-client/generated/SessionContextView";
import type { ConcurrencyMode } from "@source-inc/gents-desktop-client/generated/ConcurrencyMode";
import type { Task } from "@source-inc/gents-desktop-client/generated/Task";
import type { Trigger } from "@source-inc/gents-desktop-client/generated/Trigger";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client/generated/RenderedTimelineItem";

const AGENT_DID = "did:key:z6MkBombadilAgent";
const DEFAULT_BEHAVIOR_ID = "default";
const STARTED_AT = "2026-06-17T00:00:00.000Z";
const THIRTY_DAYS_AGO = new Date(Date.now() - 30 * 86_400_000).toISOString();
const TWO_HOURS_AGO = new Date(Date.now() - 2 * 3_600_000).toISOString();

type HarnessSessionTimestamps = { createdAt: string | null; updatedAt: string | null };

function harnessSessionContext(
  overrides: Partial<SessionContextView> = {},
): SessionContextView {
  return {
    transcriptTotalsExact: true,
    estimatedDurableTokens: 0,
    estimatedConversationTokens: 0,
    contextWindow: 128_000,
    compactionThreshold: 0.75,
    compactionThresholdTokens: 96_000,
    compactionStrategy: "StripThenSummarize",
    durableMessageCount: 0,
    providerMessageCount: 0,
    totalCompactedMessages: 0,
    compactions: [],
    lastRequest: null,
    ...overrides,
  };
}

function harnessResponseView(overrides: Partial<ResponseView> = {}): ResponseView {
  return {
    status: null,
    content: null,
    reasoning: null,
    errorMessage: null,
    tokenCount: null,
    materializedMessageSequence: null,
    materializedAt: null,
    interruptedAt: null,
    completedAt: null,
    cancelCause: null,
    backendId: null,
    ...overrides,
  };
}

function harnessAssistantItem(
  item: Omit<Extract<RenderedTimelineItem, { kind: "assistantMessage" }>, "kind">,
): RenderedTimelineItem {
  return { kind: "assistantMessage", ...item };
}

export type DesktopUiHarnessScenario =
  | "default"
  | "empty-fleet"
  | "loading"
  | "bridge-unavailable"
  | "save-error"
  | "backend-health-error"
  | "backend-unavailable"
  | "mailbox-overflow"
  | "long-content"
  | "active-turn"
  | "cascade-turn"
  | "coding"
  | "mobile-performance"
  | "session-hydration"
  | "sync-offline"
  | "sync-failed";

export type DesktopUiHarnessOptions = {
  scenario?: string | null;
};

export type MobilePerformanceBridgeCall = {
  command: string;
  durationMs: number;
  requestBytes: number;
  responseBytes: number;
};

export type MobilePerformanceCommit = {
  id: string;
  phase: "mount" | "update" | "nested-update";
  actualDurationMs: number;
  baseDurationMs: number;
  startTimeMs: number;
  commitTimeMs: number;
};

export type MobilePerformanceHarnessSnapshot = {
  bridgeCalls: MobilePerformanceBridgeCall[];
  commits: MobilePerformanceCommit[];
  updateEvents: number;
};

export type MobilePerformanceHarnessController = {
  fixture: typeof MOBILE_PERFORMANCE_FIXTURE;
  reset(): void;
  snapshot(): MobilePerformanceHarnessSnapshot;
  recordCommit(commit: MobilePerformanceCommit): void;
  streamUpdate(): number;
  streamBurst(count: number): number;
};

export type SessionSyncHarnessController = {
  progress(mergedCount: number, servedCount: number): void;
  complete(): void;
  fail(): void;
  observe(): void;
  retryCount(): number;
};

type DesktopUiHarness = {
  adapter: DesktopApiAdapter;
  listenerFactory: DesktopClientUpdatedListenerFactory;
  scenario: DesktopUiHarnessScenario;
  performance: MobilePerformanceHarnessController | null;
  sessionSync: SessionSyncHarnessController;
};

export const MOBILE_PERFORMANCE_FIXTURE = {
  id: "mobile-interactions-v1",
  sessionIndexCount: 120,
  shortSessionTimelineItems: 1,
  largeSessionTimelineItems: 600,
  transcriptPageSize: 40,
  streamUpdateCount: 50,
  repeatedNavigationCount: 10,
} as const;

export function createDesktopUiHarness(
  options: DesktopUiHarnessOptions = {},
): DesktopUiHarness {
  const scenario = normalizeScenario(options.scenario);
  const listeners = new Set<DesktopClientUpdatedHandler>();
  const sessions = new Map<string, DesktopSessionSnapshot>();
  const sessionLineage = new Map<
    string,
    {
      taskId?: string | null;
      taskName?: string | null;
      triggerId?: string | null;
      triggerKind?: string | null;
    }
  >();
  const sessionTimestamps = new Map<string, HarnessSessionTimestamps>([
    ["session-intro", { createdAt: THIRTY_DAYS_AGO, updatedAt: TWO_HOURS_AGO }],
  ]);
  let requestSeq = 1;
  let sessionSeq = 1;
  let rowCount = 42;
  let deployment = createDeployment();
  if (scenario === "empty-fleet") {
    deployment = {
      ...deployment,
      inferenceBackends: [],
      inferenceProfiles: [],
    };
  }
  if (scenario === "backend-unavailable") {
    deployment = {
      ...deployment,
      source: "local-standard",
      behaviorReadiness: {
        ...deployment.behaviorReadiness,
        behaviors: deployment.behaviorReadiness.behaviors.map((behavior) => ({
          state: "unavailable" as const,
          behaviorId: behavior.behaviorId,
          reason: "backend_disabled" as const,
        })),
      },
      inferenceBackends: deployment.inferenceBackends.map((backend) => ({
        ...backend,
        enabled: false,
      })),
    };
  }
  if (scenario === "mailbox-overflow") {
    deployment = {
      ...deployment,
      mailboxItems: [
        {
          itemId: "mailbox-mobile",
          itemKey: "mailbox-mobile-key",
          requesterDid: "did:key:z6MkRequesterWithAnUnbrokenIdentifierForMobile",
          agentDid: AGENT_DID,
          status: "pending",
          kind: "notification",
          action: "ack",
          title:
            "LongUnbrokenMailboxTitleThatMustWrapInsideTheMobileSidebarInsteadOfClippingActions",
          summary: "A mailbox item with adversarial mobile-width content.",
          payload:
            "https://example.invalid/a/very/long/unbroken/payload/path/that/must/stay/inside/the/sidebar",
          sourceKind: "AgentRequest",
          sourceId: "request_01JZ6Q0Y5Q7V0MOBILE_SOURCE_IDENTIFIER_WITHOUT_BREAKS",
          sessionId: "session-intro",
          requestId: "request-intro",
          graphRunId: null,
          causeDocId: null,
          targetAgentDid: AGENT_DID,
          targetBehaviorId: DEFAULT_BEHAVIOR_ID,
          expectedCollection: "AgentResponse",
          parentItemId: null,
          deadlineAt: null,
          createdAt: STARTED_AT,
        },
      ],
    };
  }
  let removed = false;
  let provisioned = scenario !== "empty-fleet";
  let p2pStatus: "healthy" | "degraded" | "wedged" =
    scenario === "sync-offline" ? "wedged" : "healthy";
  let syncHealth: SyncHealthView = initialSyncHealth(scenario);
  let enrollmentRequests: EnrollmentRequestView[] = [];
  let hydrationRetryCalls = 0;
  let updateEvents = 0;
  let storeVersion = 1;
  let reconcileVersion = 1;
  let streamSequence = 0;
  let bridgeCalls: MobilePerformanceBridgeCall[] = [];
  let commits: MobilePerformanceCommit[] = [];

  const greeting =
    scenario === "long-content"
      ? longHarnessMessage()
      : "I am your desktop UI test agent. This seeded turn gives the transcript a stable row for duplicate-message checks.";
  const activeTurn = scenario === "active-turn" || scenario === "cascade-turn";
  sessions.set("session-intro", {
    sessionId: "session-intro",
    agentDid: AGENT_DID,
    behaviorId: DEFAULT_BEHAVIOR_ID,
    title: "introduction-and-greetings",
    previewText: greeting,
    status: activeTurn ? "processing" : "completed",
    turnState: activeTurn ? "streaming" : "completed",
    latestRequestId: "request-intro",
    goal: null,
    retryEligibility: { eligible: false, denialReason: null },
    latestResponse: harnessResponseView({
      status: activeTurn ? "streaming" : "completed",
      content: greeting,
      tokenCount: 24,
      materializedMessageSequence: 1,
      materializedAt: STARTED_AT,
      completedAt: activeTurn ? null : STARTED_AT,
      backendId: "backend-openai",
    }),
    pendingTurn: null,
    activeResponseOverlay: null,
    context:
      scenario === "long-content"
        ? harnessSessionContext({
            estimatedDurableTokens: 142_031,
            estimatedConversationTokens: 142_031,
            contextWindow: 480_000,
            compactionThresholdTokens: 360_000,
            durableMessageCount: 8,
            providerMessageCount: 8,
          })
        : harnessSessionContext(),
    timelineItems: [
      harnessAssistantItem({
        itemKey: "intro-assistant",
        sequence: 1,
        content: greeting,
        reasoning: null,
        timestamp: "2026-06-03T14:05:00Z",
      }),
      ...(activeTurn
        ? [
            {
              kind: "toolGroup" as const,
              itemKey: "live-tools",
              messageSequence: 2,
              tools: [
                {
                  itemKey: "live-exec",
                  toolName: "gents_exec",
                  statusKind: "running",
                  presentation: {
                    kind: "command" as const,
                    command: "cargo test -p gents",
                    exitCode: null,
                    timedOut: false,
                    failed: false,
                    durationMs: null,
                    cwd: null,
                    executionMode: "read_only",
                    networkMode: "disabled",
                    stdout: "",
                    stderr: "",
                    fallbackOutput: null,
                  },
                  partialOutputTail:
                    "Compiling gents v0.7.0\ntest lifecycle::claims ... ok\ntest lifecycle::persistence ... ok",
                  partialOutputSeq: 4096,
                },
              ],
            },
          ]
        : []),
      ...(scenario === "coding"
        ? [
            {
              kind: "toolGroup" as const,
              itemKey: "intro-code-tools",
              messageSequence: 1,
              tools: [
                {
                  itemKey: "intro-edit-file",
                  toolName: "edit_file",
                  statusKind: "success",
                  presentation: {
                    kind: "fileEdit" as const,
                    operation: "edit_file",
                    path: "src/parser.rs",
                    created: false,
                    replacementsApplied: 1,
                    diff: [
                      { kind: "del", text: "fn parse() -> Ast { todo!() }" },
                      { kind: "add", text: "fn parse() -> Ast { Ast::default() }" },
                    ],
                    fallbackOutput: null,
                  },
                },
                {
                  itemKey: "intro-bash",
                  toolName: "bash",
                  statusKind: "success",
                  presentation: {
                    kind: "command" as const,
                    command: "cargo test parser",
                    exitCode: 0,
                    timedOut: false,
                    failed: false,
                    durationMs: null,
                    cwd: null,
                    executionMode: "read_only",
                    networkMode: "disabled",
                    stdout: "test result: ok. 2 passed",
                    stderr: "",
                    fallbackOutput: null,
                  },
                },
                {
                  itemKey: "intro-subagent",
                  toolName: "spawn_subagent",
                  statusKind: "success",
                  presentation: {
                    kind: "subagent" as const,
                    action: "spawn",
                    name: "reviewer",
                    childRequestId: "request-reviewer",
                    description: "Review the parser change for correctness.",
                    output: "Child request created.",
                  },
                  awaitMode: "blocking",
                },
                {
                  itemKey: "intro-process",
                  toolName: "spawn_process",
                  statusKind: "running",
                  presentation: {
                    kind: "process" as const,
                    action: "spawn",
                    target: "cargo test --workspace",
                    description: "Run the complete validation suite.",
                    output: null,
                  },
                  awaitMode: "background",
                  cancelPolicy: "cascade",
                },
                {
                  itemKey: "intro-mcp",
                  toolName: "call_tool",
                  statusKind: "success",
                  presentation: {
                    kind: "mcp" as const,
                    serviceId: "github",
                    selectedToolName: "search_issues",
                    arguments: '{"query":"mobile sync"}',
                    output: '{"count":2}',
                  },
                },
              ],
            },
          ]
        : []),
    ],
  });
  if (scenario === "empty-fleet") {
    sessions.clear();
    deployment = {
      ...deployment,
      sessions: [],
      inferenceBackends: [],
      inferenceProfiles: [],
    };
  }
  if (scenario === "session-hydration") {
    sessions.set("session-remote", {
      sessionId: "session-remote",
      agentDid: AGENT_DID,
      behaviorId: DEFAULT_BEHAVIOR_ID,
      title: "Desktop-started session",
      previewText: "hello from desktop",
      status: "completed",
      turnState: "completed",
      latestRequestId: "request-remote",
      goal: null,
      retryEligibility: { eligible: false, denialReason: null },
      latestResponse: null,
      pendingTurn: null,
      activeResponseOverlay: null,
      context: harnessSessionContext(),
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: "remote-user",
          sequence: 1,
          content: "hello from desktop",
          timestamp: THIRTY_DAYS_AGO,
        },
      ],
      hydration: {
        sessionId: "session-remote",
        agentDid: AGENT_DID,
        phase: "requested",
        mergedCount: 1,
        coveredCount: 1,
        servedCount: null,
      },
    });
  }
  if (scenario === "mobile-performance") {
    sessions.set("session-large", createLargePerformanceSession());
    for (
      let index = 2;
      index < MOBILE_PERFORMANCE_FIXTURE.sessionIndexCount;
      index += 1
    ) {
      const sessionId = `session-index-${String(index).padStart(3, "0")}`;
      sessions.set(sessionId, {
        sessionId,
        agentDid: AGENT_DID,
        behaviorId: DEFAULT_BEHAVIOR_ID,
        title: `Comparable session ${String(index).padStart(3, "0")}`,
        previewText: `Durable session-index fixture row ${index}`,
        status: "completed",
        turnState: "completed",
        latestRequestId: `request-index-${index}`,
        goal: null,
        retryEligibility: { eligible: false, denialReason: null },
        latestResponse: null,
        activeResponseOverlay: null,
        pendingTurn: null,
        context: harnessSessionContext(),
        timelineItems: [],
      });
    }
    rowCount =
      MOBILE_PERFORMANCE_FIXTURE.sessionIndexCount * 2 +
      MOBILE_PERFORMANCE_FIXTURE.largeSessionTimelineItems +
      MOBILE_PERFORMANCE_FIXTURE.shortSessionTimelineItems;
  }
  syncSessions();

  function notify(reason: string, responseOnly = false) {
    updateEvents += 1;
    if (reason === "store") {
      storeVersion += 1;
      if (!responseOnly) reconcileVersion += 1;
    }
    window.setTimeout(() => {
      for (const listener of listeners) {
        void listener({
          reason,
          storeVersion,
          reconcileVersion,
          responseOnly,
        });
      }
    }, 0);
  }

  function notifyBurst(reason: string, count: number, responseOnly = false) {
    updateEvents += count;
    window.setTimeout(() => {
      for (let index = 0; index < count; index += 1) {
        if (reason === "store") {
          storeVersion += 1;
          if (!responseOnly) reconcileVersion += 1;
        }
        for (const listener of listeners) {
          void listener({
            reason,
            storeVersion,
            reconcileVersion,
            responseOnly,
          });
        }
      }
    }, 0);
  }

  function appendStreamChunk() {
    streamSequence += 1;
    const session = sessions.get("session-large");
    if (!session) {
      throw new Error("mobile performance fixture lost session-large");
    }
    let liveContent = "";
    const timelineItems = session.timelineItems.map((item) =>
      item.kind === "liveAssistant" && item.itemKey === "large-live"
        ? (() => {
            liveContent = `${item.content ?? ""} stream-chunk-${streamSequence}`;
            return { ...item, content: liveContent };
          })()
        : item,
    );
    sessions.set("session-large", {
      ...session,
      previewText: `stream-chunk-${streamSequence}`,
      timelineItems,
      latestResponse: session.latestResponse
        ? { ...session.latestResponse, content: liveContent }
        : null,
      activeResponseOverlay: harnessResponseView({
        ...session.activeResponseOverlay,
        status: "streaming",
        content: liveContent,
        reasoning: null,
      }),
    });
    return streamSequence;
  }

  function snapshot() {
    const deployments = !provisioned || removed ? [] : [deployment];
    const health = {
      status: p2pStatus,
      connectedPeerCount: p2pStatus === "healthy" ? 1 : 0,
      replicatorCount: p2pStatus === "healthy" ? 1 : 0,
      consecutiveFailures: p2pStatus === "healthy" ? 0 : 3,
      lastOkAt: p2pStatus === "healthy" ? STARTED_AT : null,
      lastError: p2pStatus === "healthy" ? null : "fixture transport unavailable",
      lastFailureAt: p2pStatus === "healthy" ? null : STARTED_AT,
    };
    const next: DesktopClientSnapshot = {
      bootstrap: {
        defaultAgentHome: "/tmp/gents-bombadil/agent",
        initAgentName: "Bombadil UI Agent",
        initAgentDid: AGENT_DID,
        initToolCeiling: "ReadWrite",
        initToolRoot: "/tmp/gents-bombadil/workspace",
        desktopHome: "/tmp/gents-bombadil/desktop",
        peerDirectoryPath: "/tmp/gents-bombadil/peers.json",
        nodeDataDir: "/tmp/gents-bombadil/node",
        logFilePath: "/tmp/gents-bombadil/desktop.log",
        agentHomeExists: true,
        desktopHomeExists: true,
        peerDirectoryExists: true,
        clientStateExists: true,
        savedPeers:
          !provisioned
            ? []
            : [
                {
                  peerId: deployment.peerId,
                  label: deployment.label,
                  agentDid: deployment.agentDid,
                  addr: deployment.addr,
                  graphql: deployment.graphql,
                  source: deployment.source,
                },
              ],
      },
      client: {
        localPeerId: "peer-bombadil-local",
        listenAddresses: ["/ip4/127.0.0.1/tcp/9292"],
        p2pHealth: health,
        syncHealth,
        enrollmentRequests,
        bootstrapErrors: [],
        lastMutationError: null,
        focusedRequestId: null,
        configuredPeerCount: deployments.length,
        dialedPeerCount: deployments.length,
        peerIssueCount: 0,
        rowCount,
        approxSerializedBytes: rowCount * 512,
        deployments,
      },
    };
    return clone(next);
  }

  function syncSessions() {
    deployment = {
      ...deployment,
      sessions: Array.from(sessions.values()).map((session) => {
        const lineage = sessionLineage.get(session.sessionId);
        const provenance: SessionProvenance | null = lineage?.taskId
          ? {
              task_id: lineage.taskId,
              graph_run_id: null,
              parent_request_doc_id: null,
              fork: null,
            }
          : null;
        const summary: SessionSummary = {
          sessionId: session.sessionId,
          agentDid: session.agentDid ?? AGENT_DID,
          requesterDid: null,
          latestRequestDocId: null,
          closedAt: null,
          tags: [],
          provenance,
          title: session.title,
          previewText: session.previewText,
          status: session.status,
          behaviorId: session.behaviorId,
          latestRequestId: session.latestRequestId,
          taskId: lineage?.taskId ?? null,
          taskName: lineage?.taskName ?? null,
          triggerId: lineage?.triggerId ?? null,
          triggerKind: lineage?.triggerKind ?? null,
          createdAt: sessionTimestamps.get(session.sessionId)?.createdAt ?? null,
          updatedAt: sessionTimestamps.get(session.sessionId)?.updatedAt ?? null,
          turnState: session.turnState,
          messageCount: session.timelineItems.filter(
            (item) => item.kind === "userMessage" || item.kind === "assistantMessage",
          ).length,
          toolCallCount: session.timelineItems.filter(
            (item) => item.kind === "toolGroup",
          ).length,
        };
        return summary;
      }),
    };
  }

  function createSessionFromPrompt(
    prompt: string,
    behaviorId?: string | null,
    lineage?: {
      taskId?: string | null;
      taskName?: string | null;
      triggerId?: string | null;
      triggerKind?: string | null;
    },
  ) {
    const sessionId = `session-${++sessionSeq}`;
    const requestId = `request-${++requestSeq}`;
    const title = prompt.trim().slice(0, 48) || "manual-task-run";
    const response = `Bombadil harness response ${requestSeq}: received "${title}".`;
    const now = new Date().toISOString();
    const session: DesktopSessionSnapshot = {
      sessionId,
      agentDid: deployment.agentDid,
      behaviorId: behaviorId || deployment.agentPrincipal.defaultBehaviorId,
      title,
      previewText: response,
      status: "completed",
      turnState: "completed",
      latestRequestId: requestId,
      goal: null,
      retryEligibility: { eligible: false, denialReason: null },
      latestResponse: harnessResponseView({
        status: "completed",
        content: response,
        tokenCount: 32,
        materializedMessageSequence: 2,
        materializedAt: now,
        completedAt: now,
        backendId: "backend-openai",
      }),
      pendingTurn: null,
      activeResponseOverlay: null,
      context: harnessSessionContext(),
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: `${requestId}-user`,
          sequence: 1,
          content: prompt,
          timestamp: now,
        },
        harnessAssistantItem({
          itemKey: `${requestId}-assistant`,
          sequence: 2,
          content: response,
          reasoning: null,
          timestamp: now,
        }),
      ],
    };
    sessions.set(sessionId, session);
    sessionTimestamps.set(sessionId, { createdAt: now, updatedAt: now });
    if (lineage) {
      sessionLineage.set(sessionId, lineage);
    }
    rowCount += 4;
    syncSessions();
    notify("store");
    return { session, requestId };
  }

  const adapter: DesktopApiAdapter = {
    async fetchDesktopSnapshot() {
      if (scenario === "bridge-unavailable") {
        throw new Error(
          "Desktop native bridge is unavailable in the UI harness scenario.",
        );
      }
      if (scenario === "loading") {
        await wait(250);
      }
      return snapshot();
    },
    async initLocalStandardRuntime(request) {
      const label = request.label.trim() || "Bombadil UI Agent";
      provisioned = true;
      deployment = {
        ...deployment,
        label,
        agentPrincipal: { ...deployment.agentPrincipal, displayName: label },
      };
      const summary: InitSummary = {
        status: "ready",
        source: "bombadil-harness",
        agentHome: "/tmp/gents-bombadil/agent",
        desktopHome: "/tmp/gents-bombadil/desktop",
        peerDirectory: "/tmp/gents-bombadil/peers.json",
        label,
        agentName: label,
        agentDid: AGENT_DID,
        graphql: "http://127.0.0.1:9181/api/v0/graphql",
        p2pTransport: "memory",
        p2pPeerId: "peer-bombadil-local",
        p2pListenAddress: "/ip4/127.0.0.1/tcp/9292",
        peerRecordId: "peer-record-bombadil",
        nextSteps: [],
      };
      return summary;
    },
    async startDesktopClient() {
      notify("runtime");
      return snapshot();
    },
    async shutdownDesktopClient() {
      notify("runtime");
      return snapshot();
    },
    async setSelectedAgent() {
      return undefined;
    },
    async removePeer(peerId) {
      if (peerId !== deployment.peerId) {
        throw new Error(`peer ${peerId} not found`);
      }
      removed = true;
      notify("peers");
      return snapshot();
    },
    async renamePeer(peerId, label) {
      if (peerId !== deployment.peerId) {
        throw new Error(`peer ${peerId} not found`);
      }
      deployment = { ...deployment, label };
      notify("peers");
      return snapshot();
    },
    async listWorkspace(subpath) {
      const path = (subpath ?? "").replace(/^\/+|\/+$/g, "");
      if (path.includes("..")) {
        throw new Error("path escapes the workspace root");
      }
      const tree: Record<
        string,
        { name: string; kind: "dir" | "file"; size: number | null }[]
      > = {
        "": [
          { name: "src", kind: "dir", size: null },
          { name: "Cargo.toml", kind: "file", size: 812 },
          { name: "README.md", kind: "file", size: 2048 },
        ],
        src: [
          { name: "lib.rs", kind: "file", size: 4096 },
          { name: "main.rs", kind: "file", size: 1024 },
        ],
      };
      const entries = tree[path];
      if (!entries) {
        throw new Error(`cannot list ${path}: no such directory`);
      }
      return {
        root: "/tmp/agent-tool-root",
        subpath: path,
        entries,
        truncated: false,
      };
    },
    async resendRequest(requestId) {
      return { requestId: `${requestId}-resend`, sessionId: "session-intro" };
    },
    async retryRequest(requestId) {
      return {
        requestId: `${requestId}-retry`,
        sessionId: "session-intro",
        agentDid: AGENT_DID,
        behaviorId: DEFAULT_BEHAVIOR_ID,
      };
    },
    async fetchRequestTimeline(agentDid, requestId) {
      if (agentDid !== AGENT_DID) {
        throw new Error(`no deployment for ${agentDid}`);
      }
      return {
        request_id: requestId,
        session_id: "session-intro",
        agent_did: agentDid,
        behavior_id: DEFAULT_BEHAVIOR_ID,
        child_request_ids: [],
        events: [
          {
            kind: "request",
            request_id: requestId,
            lifecycle_state: "Completed",
            timestamp: STARTED_AT,
          },
          {
            kind: "message",
            role: "user",
            content: "hello there",
            sequence: 1,
            session_id: "session-intro",
            timestamp: STARTED_AT,
          },
          {
            kind: "tool_call",
            tool_name: "gents_exec",
            tool_call_id: "tc-1",
            session_id: "session-intro",
            status: "completed",
            lifecycle_state: "Completed",
            args: "{}",
            result: "ok",
            started_at: STARTED_AT,
          },
          {
            kind: "response",
            status: "materialized",
            session_id: "session-intro",
            timestamp: STARTED_AT,
          },
        ],
      };
    },
    async explainToolSurface(agentDid, behaviorId) {
      if (agentDid !== AGENT_DID) {
        throw new Error(
          "tool-surface explanation for remote agents is not yet supported",
        );
      }
      const behavior = deployment.behaviors.find(
        (candidate) => candidate.behaviorId === behaviorId,
      );
      const context = deployment.contexts.find(
        (candidate) => candidate.context_id === behavior?.contextId,
      );
      return {
        behaviorId,
        enabled: behavior?.enabled ?? true,
        contextId: context?.context_id ?? null,
        toolsId: context?.tools_id ?? null,
        toolsSource: "context",
        ceilingSource: "init_json",
        mcpServicesOnline: true,
        surface: {
          tool_names: ["read_file", "list_files", "gents_exec"],
          included: { read_file: ["ceiling allows readonly file tools"] },
          excluded: { write_file: ["ceiling is readonly"] },
          unavailable: {},
          warnings: [],
        },
      };
    },
    async fetchNetworkStatus() {
      return {
        localPeerId: "12D3KooWBombadilLocalPeer",
        localPeerIdError: null,
        listenAddresses: ["/ip4/127.0.0.1/tcp/9292/p2p/12D3KooWBombadilLocalPeer"],
        listenAddressesError: null,
        connectedPeers: [deployment.peerId],
        connectedPeersError: null,
        replicators: [
          {
            peerId: deployment.peerId,
            address: deployment.addr,
            collections: ["AgentRequest", "AgentResponse", "AgentMessage"],
            status: 0,
            lastStatusChange: TWO_HOURS_AGO,
          },
        ],
        replicatorsError: null,
        savedPeers: [
          {
            peerId: deployment.peerId,
            label: deployment.label,
            addr: deployment.addr,
            agentDid: deployment.agentDid,
            source: deployment.source,
          },
        ],
      };
    },
    async fetchPeerStatus() {
      return {
        label: "Bombadil UI Agent",
        agentDid: AGENT_DID,
        addr: "/ip4/127.0.0.1/tcp/9292",
        graphql: "http://127.0.0.1:9181/api/v0/graphql",
      };
    },
    async requestStatusEnrollment() {
      const request: EnrollmentRequestView = {
        requestId: "enrollment-request-harness",
        networkId: "network-harness",
        adminDid: AGENT_DID,
        serverPeer: "peer-enrollment-pending",
        serverLabel: "Bombadil UI Agent",
        ownerAgent: AGENT_DID,
        state: "pending_approval",
        expiresAt: "2026-09-03T15:00:00.000Z",
      };
      enrollmentRequests = [request];
      return request;
    },
    async repairP2P() {
      p2pStatus = "healthy";
      syncHealth = {
        ...syncHealth,
        state: "healthy",
        connectedPeerCount: 1,
        lastError: null,
      };
      return snapshot();
    },
    async fetchSessionSnapshot(_sessionId, _agentDid, _requestId, timelinePage) {
      const sessionId = _sessionId;
      const session = sessions.get(sessionId);
      if (!session) return null;
      const snapshot = clone(session);
      snapshot.projectionRevision = { storeVersion, reconcileVersion };
      if (!timelinePage) return snapshot;

      const totalItems = snapshot.timelineItems.length;
      const limit = Math.max(1, Math.min(80, timelinePage.limit ?? 40));
      const end = timelinePage.beforeItemKey
        ? snapshot.timelineItems.findIndex(
            (item) => item.itemKey === timelinePage.beforeItemKey,
          )
        : totalItems;
      if (end < 0) {
        throw new Error(
          `session timeline cursor is no longer present: ${timelinePage.beforeItemKey}`,
        );
      }
      const start = Math.max(0, end - limit);
      snapshot.timelineItems = snapshot.timelineItems.slice(start, end);
      snapshot.timelinePage = {
        totalItems,
        pageItems: snapshot.timelineItems.length,
        hasOlder: start > 0,
        hasNewer: end < totalItems,
        oldestItemKey: snapshot.timelineItems[0]?.itemKey ?? null,
        newestItemKey:
          snapshot.timelineItems[snapshot.timelineItems.length - 1]?.itemKey ?? null,
      };
      return snapshot;
    },
    async retrySessionHydration(sessionId, agentDid) {
      hydrationRetryCalls += 1;
      const session = sessions.get(sessionId);
      const resolvedAgentDid = agentDid ?? session?.agentDid;
      if (
        !session ||
        session.agentDid !== resolvedAgentDid ||
        session.hydration?.phase !== "failed"
      ) {
        throw new Error(
          "session hydration retry requires a failed attempt for the selected session",
        );
      }
      const hydration = {
        ...session.hydration,
        phase: "requested" as const,
        mergedCount: session.timelineItems.length,
        coveredCount: session.timelineItems.length,
        servedCount: null,
      };
      sessions.set(sessionId, { ...session, hydration });
      notify("store");
    },
    async fetchSessionLiveDelta(request) {
      const session = sessions.get(request.sessionId);
      if (!session || session.latestRequestId !== request.requestId) return null;
      const revision = { storeVersion, reconcileVersion };
      if (request.baseReconcileVersion !== reconcileVersion) {
        return {
          outcome: "snapshotRequired",
          revision,
          requestId: request.requestId,
          progressSeq: streamSequence,
          turnState: session.turnState,
          status: session.latestResponse?.status ?? null,
          content: null,
          reasoning: null,
        };
      }
      const response = session.activeResponseOverlay ?? session.latestResponse;
      const content = harnessLiveTextPatch(
        response?.content ?? "",
        request.baseContentByteLen,
        request.baseContentHash,
      );
      const reasoning = harnessLiveTextPatch(
        response?.reasoning ?? "",
        request.baseReasoningByteLen,
        request.baseReasoningHash,
      );
      return {
        outcome:
          content.mode === "unchanged" && reasoning.mode === "unchanged"
            ? "unchanged"
            : "delta",
        revision,
        requestId: request.requestId,
        progressSeq: streamSequence,
        turnState: session.turnState,
        status: session.latestResponse?.status ?? null,
        content,
        reasoning,
      };
    },
    async sendChatMessage(request) {
      const content = request.content.trim();
      if (!content) {
        throw new Error("message content is required");
      }

      if (request.sessionId && sessions.has(request.sessionId)) {
        const existing = sessions.get(request.sessionId)!;
        const nextSequence = existing.timelineItems.length + 1;
        const requestId = `request-${++requestSeq}`;
        const response = `Bombadil harness response ${requestSeq}: received "${content.slice(
          0,
          48,
        )}".`;
        const updated: DesktopSessionSnapshot = {
          ...existing,
          previewText: response,
          status: "completed",
          turnState: "completed",
          latestRequestId: requestId,
          latestResponse: harnessResponseView({
            status: "completed",
            content: response,
            tokenCount: 32,
            materializedMessageSequence: nextSequence + 1,
            materializedAt: new Date().toISOString(),
            completedAt: new Date().toISOString(),
            backendId: "backend-openai",
          }),
          timelineItems: [
            ...existing.timelineItems,
            {
              kind: "userMessage",
              itemKey: `${requestId}-user`,
              sequence: nextSequence,
              content,
              timestamp: new Date().toISOString(),
            },
            harnessAssistantItem({
              itemKey: `${requestId}-assistant`,
              sequence: nextSequence + 1,
              content: response,
              reasoning: null,
              timestamp: new Date().toISOString(),
            }),
          ],
        };
        sessions.set(request.sessionId, updated);
        rowCount += 4;
        syncSessions();
        notify("store");
        return {
          sessionId: request.sessionId,
          requestId,
          agentDid: request.agentDid,
          behaviorId: request.behaviorId ?? null,
        };
      }

      const { session, requestId } = createSessionFromPrompt(
        content,
        request.behaviorId,
      );
      const result: TaskRunResult = {
        requestDocId: `${requestId}-doc`,
        requestId,
        sessionId: session.sessionId,
        agentDid: request.agentDid,
        behaviorId: request.behaviorId || DEFAULT_BEHAVIOR_ID,
        lifecycleState: "completed",
      };
      return result;
    },
    async renameSession(request) {
      const session = sessions.get(request.sessionId);
      if (session) {
        sessions.set(request.sessionId, {
          ...session,
          title: request.title.trim() || session.title,
        });
        syncSessions();
        notify("store");
      }
    },
    async listMailbox() {
      return deployment.mailboxItems;
    },
    async startMailboxRequest(itemId) {
      const item = deployment.mailboxItems.find((entry) => entry.itemId === itemId);
      if (!item) {
        throw new Error(`mailbox item ${itemId} not found`);
      }
      return item;
    },
    async dismissMailboxItem() {
      return undefined;
    },
    async applyConfigComponents(request) {
      if (scenario === "save-error") {
        throw new Error("Harness rejected config apply for sad-path coverage.");
      }
      deployment = {
        ...deployment,
        behaviorConfigs: [
          ...deployment.behaviorConfigs.filter(
            (behavior) =>
              !request.document.agent_behaviors?.some(
                (candidate) => candidate.behavior_id === behavior.behavior_id,
              ),
          ),
          ...(request.document.agent_behaviors ?? []),
        ],
        contexts: [
          ...deployment.contexts.filter(
            (context) =>
              !request.document.contexts?.some(
                (candidate) => candidate.context_id === context.context_id,
              ),
          ),
          ...(request.document.contexts ?? []),
        ],
        compactions: [
          ...deployment.compactions.filter(
            (compaction) =>
              !request.document.compactions?.some(
                (candidate) => candidate.compaction_id === compaction.compaction_id,
              ),
          ),
          ...(request.document.compactions ?? []),
        ],
        tools: [
          ...deployment.tools.filter(
            (entry) =>
              !request.document.tools?.some(
                (candidate) => candidate.tools_id === entry.tools_id,
              ),
          ),
          ...(request.document.tools ?? []),
        ],
        subagentTargets: [
          ...deployment.subagentTargets.filter(
            (target) =>
              !request.document.subagent_targets?.some(
                (candidate) => candidate.target_id === target.target_id,
              ),
          ),
          ...(request.document.subagent_targets ?? []),
        ],
        skills: [
          ...deployment.skills.filter(
            (skill) =>
              !request.document.skills?.some(
                (candidate) => candidate.skill_id === skill.skillId,
              ),
          ),
          ...(request.document.skills ?? []).map((document) => ({
            skillId: document.skill_id,
            agentDid: document.agent_did,
            name: document.name ?? null,
            description: document.description ?? null,
            instructions: document.instructions ?? null,
            toolRefs: document.tool_refs ?? [],
            displayName: document.display_name ?? null,
            enabled: document.enabled ?? true,
            createdAt: document.created_at ?? null,
          })),
        ],
        inferenceProfiles: [
          ...deployment.inferenceProfiles.filter(
            (profile) =>
              !request.document.inference_profiles?.some(
                (candidate) => candidate.profile_id === profile.profile_id,
              ),
          ),
          ...(request.document.inference_profiles ?? []),
        ],
        toolServiceRegistries: [
          ...deployment.toolServiceRegistries.filter(
            (service) =>
              !request.document.tool_service_registries?.some(
                (candidate) => candidate.service_id === service.service_id,
              ),
          ),
          ...(request.document.tool_service_registries ?? []),
        ],
        tasks: [
          ...deployment.tasks.filter(
            (task) =>
              !request.document.tasks?.some(
                (candidate) => candidate.task_id === task.taskId,
              ),
          ),
          ...(request.document.tasks ?? []).map((document) => ({
            taskId: document.task_id,
            name: document.display_name ?? null,
            description: document.description ?? null,
            behaviorId: document.behavior_id,
            promptTemplate: document.prompt_template,
            goalObjectiveTemplate: document.goal_objective_template ?? null,
            goalTokenBudget: document.goal_token_budget ?? null,
            enabled: document.enabled ?? true,
            outputSchemaRef: document.output_schema_ref ?? null,
            hooks: document.hooks ?? [],
            tags: document.tags ?? [],
            recentRuns: {
              totalFires: 0,
              lastAttemptAt: null,
              lastStatus: null,
              lastError: null,
              scheduleCount: 0,
              eventCount: 0,
            },
            runHistory: [],
          })),
        ],
        schedules: [
          ...deployment.schedules.filter(
            (schedule) =>
              !request.document.schedules?.some(
                (candidate) => candidate.schedule_id === schedule.schedule_id,
              ),
          ),
          ...(request.document.schedules ?? []),
        ],
        eventSources: [
          ...deployment.eventSources.filter(
            (source) =>
              !request.document.event_sources?.some(
                (candidate) => candidate.event_source_id === source.event_source_id,
              ),
          ),
          ...(request.document.event_sources ?? []),
        ],
        triggers: [
          ...deployment.triggers.filter(
            (trigger) =>
              !request.document.triggers?.some(
                (candidate) => candidate.trigger_id === trigger.config.trigger_id,
              ),
          ),
          ...(request.document.triggers ?? []).map((config) => ({
            config,
            nextRunAt: null,
            lastAttemptAt: null,
            lastFiredSourceDocId: null,
            lastStatus: null,
            lastError: null,
            fireCount: 0,
          })),
        ],
      };
      notify("config");
      return snapshot();
    },
    async patchConfigComponents(request) {
      for (const patch of request.patches) {
        if (patch.collection === "InferenceBackend") {
          deployment = {
            ...deployment,
            inferenceBackends: deployment.inferenceBackends.map((backend) =>
              backend.backendId === patch.id
                ? { ...backend, ...patch.changes }
                : backend,
            ),
          };
        }
      }
      notify("config");
      return snapshot();
    },
    async saveAgentConfig(request) {
      const document: AgentPrincipal = request.document;
      deployment = {
        ...deployment,
        label: document.display_name?.trim() || deployment.label,
        agentPrincipal: {
          ...deployment.agentPrincipal,
          displayName: document.display_name?.trim() || deployment.label,
          defaultBehaviorId: document.default_behavior_id ?? null,
          enabled: document.enabled ?? deployment.agentPrincipal.enabled,
        },
        principalConfig: {
          ...document,
          display_name: document.display_name ?? deployment.label,
          default_behavior_id:
            document.default_behavior_id ?? deployment.agentPrincipal.defaultBehaviorId,
          enabled: document.enabled ?? deployment.agentPrincipal.enabled,
        },
      };
      return snapshot();
    },
    async saveBehaviorConfig(request) {
      if (scenario === "save-error") {
        throw new Error("Harness rejected behavior save for sad-path coverage.");
      }
      const document: AgentBehavior = request.document;
      const behaviorId = document.behavior_id.trim() || `behavior-${requestSeq}`;
      const nextBehavior: BehaviorView = {
        behaviorId,
        agentDid: document.agent_did,
        displayName: document.display_name?.trim() || behaviorId,
        description: document.description ?? null,
        contextId: document.context_id ?? null,
        inferenceProfileId: document.inference_profile_id,
        enabled: document.enabled ?? true,
        isDefault: behaviorId === deployment.agentPrincipal.defaultBehaviorId,
        tags: document.tags ?? [],
        createdAt: null,
      };
      deployment = {
        ...deployment,
        behaviors: upsertBy(
          deployment.behaviors,
          "behaviorId",
          behaviorId,
          nextBehavior,
        ),
        behaviorConfigs: upsertBy(
          deployment.behaviorConfigs,
          "behavior_id",
          behaviorId,
          {
            ...document,
            behavior_id: behaviorId,
            display_name: document.display_name?.trim() || behaviorId,
          },
        ),
      };
      return snapshot();
    },
    async saveSkillConfig(request) {
      const document = request.document;
      const skillId = document.skill_id.trim() || `skill-${requestSeq}`;
      const name = document.name?.trim() || skillId;
      deployment = {
        ...deployment,
        skills: upsertBy(deployment.skills ?? [], "skillId", skillId, {
          skillId,
          agentDid: document.agent_did || deployment.agentDid,
          name,
          description: document.description ?? null,
          instructions: document.instructions ?? null,
          toolRefs: document.tool_refs ?? [],
          displayName: document.display_name?.trim() || name,
          enabled: document.enabled ?? true,
          createdAt: document.created_at ?? STARTED_AT,
        }),
      };
      return snapshot();
    },
    async deleteTaskConfig(request) {
      const triggers = deployment.triggers.filter(
        (trigger) => trigger.config.task_id === request.taskId,
      ).length;
      if (triggers > 0) {
        throw new Error(
          `task "${request.taskId}" is referenced by ${triggers} trigger(s); delete or detach those first`,
        );
      }
      deployment = {
        ...deployment,
        tasks: deployment.tasks.filter((task) => task.taskId !== request.taskId),
      };
      notify("config");
      return snapshot();
    },
    async deleteScheduleConfig(request) {
      deployment = {
        ...deployment,
        schedules: deployment.schedules.filter(
          (schedule) => schedule.schedule_id !== request.scheduleId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteEventSourceConfig(request) {
      deployment = {
        ...deployment,
        eventSources: deployment.eventSources.filter(
          (source) => source.event_source_id !== request.eventSourceId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteTriggerConfig(request) {
      deployment = {
        ...deployment,
        triggers: deployment.triggers.filter(
          (trigger) => trigger.config.trigger_id !== request.triggerId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteBackendConfig(request) {
      const referencingBehaviors = deployment.behaviors.filter((behavior) => {
        const profile = deployment.inferenceProfiles.find(
          (candidate) => candidate.profile_id === behavior.inferenceProfileId,
        );
        return profile?.backend_id === request.backendId;
      });
      const referencing = referencingBehaviors.map((behavior) => behavior.behaviorId);
      if (referencing.length) {
        throw new Error(
          `backend "${request.backendId}" is referenced by inference profile(s) used by behavior(s) ${referencing.join(", ")}; point them elsewhere first`,
        );
      }
      deployment = {
        ...deployment,
        inferenceBackends: deployment.inferenceBackends.filter(
          (backend) => backend.backendId !== request.backendId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteInferenceProfileConfig(request) {
      const referencing = deployment.behaviors
        .filter((behavior) => behavior.inferenceProfileId === request.profileId)
        .map((behavior) => behavior.behaviorId);
      if (referencing.length) {
        throw new Error(
          `profile "${request.profileId}" is referenced by behavior(s) ${referencing.join(", ")}; point them elsewhere first`,
        );
      }
      deployment = {
        ...deployment,
        inferenceProfiles: deployment.inferenceProfiles.filter(
          (profile) => profile.profile_id !== request.profileId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteToolsConfig(request) {
      const referencing = deployment.behaviors
        .filter((behavior) => {
          const context = deployment.contexts.find(
            (candidate) => candidate.context_id === behavior.contextId,
          );
          return context?.tools_id === request.toolsId;
        })
        .map((behavior) => behavior.behaviorId);
      if (referencing.length) {
        throw new Error(
          `tools document "${request.toolsId}" is referenced by behavior(s) ${referencing.join(", ")}; point them elsewhere first`,
        );
      }
      deployment = {
        ...deployment,
        tools: deployment.tools.filter((entry) => entry.tools_id !== request.toolsId),
      };
      notify("config");
      return snapshot();
    },
    async deleteToolServiceConfig(request) {
      const referencing = deployment.tools
        .filter((entry) =>
          (entry.remote?.services ?? []).some(
            (service) => service.mcp_service_id === request.serviceId,
          ),
        )
        .map((entry) => entry.tools_id);
      if (referencing.length) {
        throw new Error(
          `tool service "${request.serviceId}" is selected by tools document(s) ${referencing.join(", ")}; remove it there first`,
        );
      }
      deployment = {
        ...deployment,
        toolServiceRegistries: deployment.toolServiceRegistries.filter(
          (service) => service.service_id !== request.serviceId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteBehaviorConfig(request) {
      const isDefault =
        deployment.behaviors.find(
          (behavior) => behavior.behaviorId === request.behaviorId,
        )?.isDefault ??
        deployment.agentPrincipal.defaultBehaviorId === request.behaviorId;
      if (isDefault) {
        throw new Error(
          `behavior "${request.behaviorId}" is the agent's default behavior; make another behavior the default first`,
        );
      }
      deployment = {
        ...deployment,
        behaviors: deployment.behaviors.filter(
          (behavior) => behavior.behaviorId !== request.behaviorId,
        ),
        behaviorConfigs: deployment.behaviorConfigs.filter(
          (behavior) => behavior.behavior_id !== request.behaviorId,
        ),
      };
      notify("config");
      return snapshot();
    },
    async deleteSkillConfig(request) {
      const skillId = request.skillId.trim();
      deployment = {
        ...deployment,
        skills: (deployment.skills ?? []).filter((skill) => skill.skillId !== skillId),
        contexts: deployment.contexts.map((context) => ({
          ...context,
          skill_ids: (context.skill_ids ?? []).filter((id) => id !== skillId),
        })),
      };
      return snapshot();
    },
    async saveBackendConfig(request) {
      const document = request.document;
      const backendId = document.backend_id.trim() || `backend-${requestSeq}`;
      const apiKeyEnvVar =
        document.auth.kind === "environment" ? document.auth.variable : null;
      deployment = {
        ...deployment,
        inferenceBackends: upsertBy(
          deployment.inferenceBackends,
          "backendId",
          backendId,
          {
            backendId,
            name: document.name.trim() || backendId,
            providerKind: document.provider_kind,
            openaiWireApi: document.openai_wire_api ?? null,
            endpoint: document.endpoint,
            authKind: document.auth.kind,
            connectTimeoutSecs: document.connect_timeout_secs ?? null,
            discoveryTimeoutSecs: document.discovery_timeout_secs ?? null,
            apiKeyConfigured: document.auth.kind !== "unauthenticated",
            apiKeyEnvVar,
            maxConcurrent: document.max_concurrent ?? null,
            maxQueueDepth: document.max_queue_depth ?? null,
            enabled: document.enabled ?? true,
            models: [],
            probeStatus: "healthy",
          },
        ),
      };
      return snapshot();
    },
    async saveInferenceProfileConfig(request) {
      const document = request.document;
      const profileId = document.profile_id.trim() || `profile-${requestSeq}`;
      deployment = {
        ...deployment,
        inferenceProfiles: upsertBy(
          deployment.inferenceProfiles,
          "profile_id",
          profileId,
          {
            ...document,
            profile_id: profileId,
            display_name: document.display_name?.trim() || profileId,
          },
        ),
      };
      return snapshot();
    },
    async saveToolsConfig(request) {
      const document = request.document;
      const toolsId = document.tools_id.trim() || `tools-${requestSeq}`;
      const prior = deployment.tools.find((entry) => entry.tools_id === toolsId);
      deployment = {
        ...deployment,
        tools: upsertBy(deployment.tools, "tools_id", toolsId, {
          ...prior,
          ...document,
          tools_id: toolsId,
          display_name: document.display_name?.trim() || toolsId,
        }),
      };
      return snapshot();
    },
    async saveToolServiceConfig(request) {
      const document = request.document;
      const serviceId = document.service_id.trim() || `service-${requestSeq}`;
      deployment = {
        ...deployment,
        toolServiceRegistries: upsertBy(
          deployment.toolServiceRegistries,
          "service_id",
          serviceId,
          {
            ...document,
            service_id: serviceId,
            display_name: document.display_name?.trim() || serviceId,
          },
        ),
      };
      return snapshot();
    },
    async testToolService(request) {
      if (!request.hostname || !request.mcpPort || !request.mcpPath) {
        throw new Error("complete MCP route is required");
      }
      const result: ToolServiceTestResult = {
        serviceId: request.serviceId,
        endpoint: `http://${request.hostname}:${request.mcpPort}${request.mcpPath}`,
        status: "ok",
        toolCount: 1,
        tools: [{ name: "whoami", description: "Returns bound caller identity" }],
        error: null,
      };
      return result;
    },
    async saveTaskConfig(request) {
      const document: Task = request.document;
      const taskId = document.task_id.trim() || `task-${requestSeq}`;
      deployment = {
        ...deployment,
        tasks: upsertBy(deployment.tasks, "taskId", taskId, {
          taskId,
          name: document.display_name?.trim() || taskId,
          description: document.description ?? null,
          behaviorId: document.behavior_id,
          promptTemplate: document.prompt_template,
          goalObjectiveTemplate: document.goal_objective_template ?? null,
          goalTokenBudget: document.goal_token_budget ?? null,
          enabled: document.enabled ?? true,
          outputSchemaRef: document.output_schema_ref ?? null,
          hooks: document.hooks ?? [],
          tags: document.tags ?? [],
          recentRuns: {
            totalFires: 0,
            lastAttemptAt: null,
            lastStatus: null,
            lastError: null,
            scheduleCount: 0,
            eventCount: deployment.triggers.filter(
              (trigger) =>
                trigger.config.task_id === taskId &&
                trigger.config.source.kind === "event",
            ).length,
          },
          runHistory: [],
        }),
      };
      return snapshot();
    },
    async saveScheduleConfig(request) {
      const document = request.document;
      const scheduleId = document.schedule_id.trim() || `schedule-${requestSeq}`;
      deployment = {
        ...deployment,
        schedules: upsertBy(deployment.schedules, "schedule_id", scheduleId, {
          ...document,
          schedule_id: scheduleId,
          display_name: document.display_name?.trim() || scheduleId,
        }),
      };
      return snapshot();
    },
    async runSchedule(request) {
      const schedule = deployment.schedules.find(
        (row) => row.schedule_id === request.scheduleId,
      );
      const trigger = deployment.triggers.find(
        (row) =>
          row.config.source.kind === "schedule" &&
          row.config.source.schedule_id === request.scheduleId,
      );
      if (!trigger) {
        throw new Error(
          `schedule "${request.scheduleId}" has no trigger selecting a task`,
        );
      }
      void schedule;
      return runHarnessTask(trigger.config.task_id);
    },
    async saveTriggerConfig(request) {
      const document: Trigger = request.document;
      const triggerId = document.trigger_id.trim() || `trigger-${requestSeq}`;
      const next: TriggerView = {
        config: { ...document, trigger_id: triggerId },
        nextRunAt: null,
        lastAttemptAt: null,
        lastFiredSourceDocId: null,
        lastStatus: null,
        lastError: null,
        fireCount: 0,
      };
      const index = deployment.triggers.findIndex(
        (row) => row.config.trigger_id === triggerId,
      );
      deployment = {
        ...deployment,
        triggers:
          index < 0
            ? [...deployment.triggers, next]
            : deployment.triggers.map((row, i) => (i === index ? next : row)),
      };
      return snapshot();
    },
    async runTask(request) {
      return runHarnessTask(request.taskId);
    },
    async saveEventSourceConfig(request) {
      const document = request.document;
      const sourceId = document.event_source_id.trim() || `event-source-${requestSeq}`;
      const next = { ...document, event_source_id: sourceId };
      const index = deployment.eventSources.findIndex(
        (row) => row.event_source_id === sourceId,
      );
      deployment = {
        ...deployment,
        eventSources:
          index < 0
            ? [...deployment.eventSources, next]
            : deployment.eventSources.map((row, i) => (i === index ? next : row)),
      };
      return snapshot();
    },
    async probeInferenceEndpoint() {
      return {
        reachable: true,
        latencyMs: 12,
        models: ["gpt-4.1-mini"],
        error: null,
      };
    },
    async codexLogin(agentDid) {
      const result: CodexLoginResult = {
        docId: `credential-${agentDid}-codex`,
        credentialId: "credential-codex",
        agentDid,
        provider: "codex",
        accountId: null,
        chatgptPlanType: null,
        isFedramp: false,
        accessTokenExpiresAt: new Date(Date.now() + 3_600_000).toISOString(),
        enabled: true,
      };
      return result;
    },
    async cancelCodexLogin() {},
    async grokLogin(agentDid) {
      const result: GrokLoginResult = {
        docId: `credential-${agentDid}-grok`,
        credentialId: "credential-grok",
        agentDid,
        provider: "grok",
        accessTokenExpiresAt: new Date(Date.now() + 3_600_000).toISOString(),
        enabled: true,
      };
      return result;
    },
    async cancelGrokLogin() {},
    async listSubagentTree(request) {
      const tree: SubagentTreeView = {
        rootRequestId: request.rootRequestId,
        truncated: false,
        nodes: [
          {
            requestId: request.rootRequestId,
            resolvedVia: null,
            sessionId: findSessionByRequest(request.rootRequestId)?.sessionId ?? null,
            agentDid: deployment.agentDid,
            behaviorId: deployment.agentPrincipal.defaultBehaviorId,
            lifecycleState: "completed",
            subagentDepth: 0,
            causedByParentRequestId: null,
            causedByParentToolCallId: null,
            backendId: null,
          },
        ],
        edges: [],
        partialErrors: [],
      };
      return tree;
    },
    async listBackendsWithHealth() {
      if (scenario === "backend-health-error") {
        throw new Error("Harness backend health bridge unavailable.");
      }
      const backends: InferenceBackendView[] = deployment.inferenceBackends;
      const rows: BackendHealth[] = backends.map((backend) => ({
        backendId: backend.backendId,
        name: backend.name ?? backend.backendId,
        providerKind: backend.providerKind ?? "OpenAiCompatible",
        endpoint: backend.endpoint ?? "http://127.0.0.1:8000/v1",
        enabled: backend.enabled ?? true,
        probeStatus: backend.probeStatus ?? "healthy",
        displayState: deriveDisplayState(
          backend.enabled ?? true,
          backend.probeStatus ?? "healthy",
        ),
        lastProbe: STARTED_AT,
        maxConcurrent: backend.maxConcurrent ?? 4,
        maxQueueDepth: backend.maxQueueDepth ?? 16,
        models: backend.models,
        recentCalls: [],
      }));
      return rows;
    },
    async listMcpServicesWithHealth() {
      const registries = deployment.toolServiceRegistries;
      const services: MCPServiceHealthView[] = registries.map((service) => ({
        serviceId: service.service_id,
        agentDid: deployment.agentDid,
        endpoint: `${service.hostname ?? "localhost"}:${service.mcp_port ?? 7331}${
          service.mcp_path ?? "/mcp"
        }`,
        status: "healthy",
        displayState: "healthy",
        toolCount: null,
        failureCount: 0,
        kMax: 3,
        backoffUntil: null,
        lastProbeAt: STARTED_AT,
        lastSeen: STARTED_AT,
        lastErrorClass: null,
        lastErrorMessage: null,
        updatedAt: STARTED_AT,
      }));
      return services;
    },
    async probeMcpService(serviceId) {
      const result: McpServiceProbeResult = {
        serviceId,
        status: "healthy",
        latencyMs: 3,
        lastError: null,
      };
      return result;
    },
    async fetchOperationsSnapshot(request) {
      const operations: DesktopOperationsSnapshot = {
        fetchedAt: new Date().toISOString(),
        agentDid: request.agentDid ?? deployment.agentDid,
        liveness: {
          expiredProcessingCount: 0,
          requests: [],
          activeToolCalls: [],
          activeNativeExecutorsAvailable: true,
          activeNativeExecutors: [],
        },
        livenessUnavailableReason: null,
        backgroundedTools: [],
        stuckDiagnostics: [],
        lineage: null,
      };
      return operations;
    },
    async previewInterruptCascade(request) {
      const cascadeChildren =
        scenario === "cascade-turn"
          ? [
              {
                requestId: "request_01JZ6Q0Y5Q7V0MOBILE_CASCADE_CHILD_WITHOUT_BREAKS",
                sessionId: "session_01JZ6Q0Y5Q7V0MOBILE_CASCADE_CHILD_WITHOUT_BREAKS",
                behaviorId: "ops",
                lifecycleState: "processing",
                parentRequestId: request.requestId,
                parentToolCallId:
                  "tool_call_01JZ6Q0Y5Q7V0MOBILE_CASCADE_PARENT_WITHOUT_BREAKS",
                toolName:
                  "mcp__subagent_coordinator__delegate_to_remote_behavior_without_breaks",
                awaitMode: "foreground",
                cancelPolicy: "cascade",
              },
            ]
          : [];
      const preview: CascadeCancelPreview = {
        rootRequestId: request.requestId,
        previewSignature: `preview-${request.requestId}`,
        rootState: activeTurn ? "processing" : "completed",
        willInterrupt: cascadeChildren,
        willDetach: [],
        alreadyTerminal: [
          ...(activeTurn
            ? []
            : [
                {
                  requestId: request.requestId,
                  sessionId: null,
                  behaviorId: deployment.agentPrincipal.defaultBehaviorId,
                  lifecycleState: "completed",
                  parentRequestId: null,
                  parentToolCallId: null,
                  toolName: null,
                  awaitMode: null,
                  cancelPolicy: null,
                },
              ]),
        ],
        unknownPolicy: [],
      };
      return preview;
    },
    async interruptRequest(request) {
      const result: InterruptRequestResult = {
        requestId: request.requestId,
        accepted: true,
        interruptRequestedAt: new Date().toISOString(),
        alreadyInterrupted: false,
        stalePreview: false,
        preview: null,
      };
      return result;
    },
  };

  const listenerFactory: DesktopClientUpdatedListenerFactory = async (handler) => {
    listeners.add(handler);
    return () => {
      listeners.delete(handler);
    };
  };

  function runHarnessTask(taskId: string): TaskRunResult {
    const task = deployment.tasks.find((row) => row.taskId === taskId);
    const behaviorId = deployment.agentPrincipal.defaultBehaviorId;
    if (!behaviorId) {
      throw new Error("runtime readiness did not assign a default behavior");
    }
    const { session, requestId } = createSessionFromPrompt(
      `Run task ${taskId}`,
      behaviorId,
      {
        taskId,
        taskName: task?.name ?? taskId,
      },
    );
    return {
      requestDocId: `${requestId}-doc`,
      requestId,
      sessionId: session.sessionId,
      agentDid: deployment.agentDid,
      behaviorId,
      lifecycleState: "completed",
    };
  }

  function findSessionByRequest(requestId: string) {
    return Array.from(sessions.values()).find(
      (session) => session.latestRequestId === requestId,
    );
  }

  const performance: MobilePerformanceHarnessController | null =
    scenario === "mobile-performance"
      ? {
          fixture: MOBILE_PERFORMANCE_FIXTURE,
          reset() {
            bridgeCalls = [];
            commits = [];
            updateEvents = 0;
          },
          snapshot() {
            return clone({ bridgeCalls, commits, updateEvents });
          },
          recordCommit(commit) {
            commits.push(commit);
          },
          streamUpdate() {
            const sequence = appendStreamChunk();
            syncSessions();
            notify("store", true);
            return sequence;
          },
          streamBurst(count) {
            let sequence = streamSequence;
            for (let index = 0; index < count; index += 1) {
              sequence = appendStreamChunk();
            }
            syncSessions();
            notifyBurst("store", count, true);
            return sequence;
          },
        }
      : null;

  const measuredAdapter = performance
    ? new Proxy(adapter, {
        get(target, property, receiver) {
          const value = Reflect.get(target, property, receiver);
          if (typeof value !== "function") return value;
          return async (...args: unknown[]) => {
            const startedAt = window.performance.now();
            const result = await value(...args);
            bridgeCalls.push({
              command: String(property),
              durationMs: window.performance.now() - startedAt,
              requestBytes: serializedBytes(args),
              responseBytes: serializedBytes(result),
            });
            return result;
          };
        },
      })
    : adapter;

  function remoteHydration(
    phase: NonNullable<DesktopSessionSnapshot["hydration"]>["phase"],
    mergedCount: number,
    servedCount: number | null,
  ) {
    return {
      sessionId: "session-remote",
      agentDid: AGENT_DID,
      phase,
      mergedCount,
      coveredCount:
        servedCount == null ? mergedCount : Math.min(mergedCount, servedCount),
      servedCount,
    };
  }

  const sessionSync: SessionSyncHarnessController = {
    observe() {
      notify("store");
    },
    retryCount() {
      return hydrationRetryCalls;
    },
    progress(mergedCount, servedCount) {
      const session = sessions.get("session-remote");
      if (!session) return;
      const timelineItems = [...session.timelineItems];
      if (
        mergedCount >= 2 &&
        !timelineItems.some((item) => item.itemKey === "remote-assistant")
      ) {
        timelineItems.push(
          harnessAssistantItem({
            itemKey: "remote-assistant",
            sequence: 2,
            content: "history arrived from the desktop",
            reasoning: null,
            timestamp: TWO_HOURS_AGO,
          }),
        );
      }
      const lastItem = timelineItems.at(-1);
      sessions.set("session-remote", {
        ...session,
        previewText:
          lastItem && "content" in lastItem
            ? (lastItem.content ?? session.previewText)
            : session.previewText,
        timelineItems,
        hydration: remoteHydration("serving", mergedCount, servedCount),
      });
      syncSessions();
      notify("store");
    },
    complete() {
      const session = sessions.get("session-remote");
      if (!session) return;
      const servedCount =
        session.hydration?.servedCount ?? Math.max(session.timelineItems.length, 2);
      const hydration = remoteHydration("complete", servedCount, servedCount);
      sessions.set("session-remote", { ...session, hydration });
      notify("store");
    },
    fail() {
      const session = sessions.get("session-remote");
      if (!session) return;
      const hydration = remoteHydration(
        "failed",
        session.timelineItems.length,
        session.hydration?.servedCount ?? null,
      );
      sessions.set("session-remote", { ...session, hydration });
      notify("store");
    },
  };

  return {
    adapter: measuredAdapter,
    listenerFactory,
    scenario,
    performance,
    sessionSync,
  };
}

function createLargePerformanceSession(): DesktopSessionSnapshot {
  const filler =
    "bounded durable transcript fixture with markdown `code`, stable prose, and comparable byte shape";
  const timelineItems = Array.from(
    { length: MOBILE_PERFORMANCE_FIXTURE.largeSessionTimelineItems },
    (_, index) => {
      if (index === MOBILE_PERFORMANCE_FIXTURE.largeSessionTimelineItems - 1) {
        return {
          kind: "liveAssistant" as const,
          itemKey: "large-live",
          content: "stream-start",
          reasoning: null,
        };
      }
      return index % 2 === 0
        ? {
            kind: "userMessage" as const,
            itemKey: `large-user-${index}`,
            requestId: `large-request-${index}`,
            sequence: index,
            content: `User fixture row ${index}: ${filler}`,
            timestamp: STARTED_AT,
          }
        : {
            kind: "assistantMessage" as const,
            itemKey: `large-assistant-${index}`,
            sequence: index,
            content: `Assistant fixture row ${index}: ${filler}`,
            reasoning: index % 10 === 1 ? `Reasoning fixture ${index}` : null,
            timestamp: STARTED_AT,
          };
    },
  );
  return {
    sessionId: "session-large",
    agentDid: AGENT_DID,
    behaviorId: DEFAULT_BEHAVIOR_ID,
    title: "Large local transcript — 600 timeline items",
    previewText: "stream-start",
    status: "processing",
    turnState: "streaming",
    latestRequestId: "large-request-live",
    goal: null,
    retryEligibility: { eligible: false, denialReason: null },
    latestResponse: harnessResponseView({
      status: "streaming",
      content: "stream-start",
      tokenCount: 1,
      backendId: "backend-openai",
    }),
    pendingTurn: null,
    activeResponseOverlay: harnessResponseView({
      status: "streaming",
      content: "stream-start",
    }),
    context: harnessSessionContext(),
    timelineItems,
  };
}

function serializedBytes(value: unknown) {
  try {
    return new TextEncoder().encode(JSON.stringify(value)).byteLength;
  } catch {
    return 0;
  }
}

function harnessLiveTextHash(value: string) {
  let hash = 0x811c9dc5;
  for (const byte of new TextEncoder().encode(value)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

function harnessLiveTextPatch(value: string, baseByteLen: number, baseHash: string) {
  const bytes = new TextEncoder().encode(value);
  const prefix = new TextDecoder("utf-8", { fatal: true });
  let prefixValue: string | null = null;
  try {
    prefixValue = prefix.decode(bytes.slice(0, baseByteLen));
  } catch {
    prefixValue = null;
  }
  const prefixMatches =
    baseByteLen <= bytes.byteLength &&
    prefixValue !== null &&
    harnessLiveTextHash(prefixValue) === baseHash;
  const mode =
    prefixMatches && baseByteLen === bytes.byteLength
      ? "unchanged"
      : prefixMatches
        ? "append"
        : "replace";
  return {
    mode,
    value:
      mode === "unchanged"
        ? ""
        : mode === "append"
          ? new TextDecoder().decode(bytes.slice(baseByteLen))
          : value,
    byteLen: bytes.byteLength,
    hash: harnessLiveTextHash(value),
  };
}

function createDeployment(): DeploymentView {
  return {
    peerId: "peer-bombadil-local",
    label: "Bombadil UI Agent",
    agentDid: AGENT_DID,
    addr: "/ip4/127.0.0.1/tcp/9292",
    source: "bombadil-harness",
    graphql: "http://127.0.0.1:9181/api/v0/graphql",
    dialSucceeded: true,
    chatSafe: true,
    behaviorReadiness: {
      source: { state: "current" },
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: STARTED_AT,
      behaviors: [
        { state: "ready", behaviorId: DEFAULT_BEHAVIOR_ID },
        { state: "ready", behaviorId: "ops" },
      ],
    },
    routes: [],
    pairing: [],
    lastError: null,
    agentPrincipal: {
      agentDid: AGENT_DID,
      displayName: "Bombadil UI Agent",
      defaultBehaviorId: DEFAULT_BEHAVIOR_ID,
      enabled: true,
      createdAt: STARTED_AT,
      createdBy: "bombadil",
    },
    runtime: {
      reconcilePhase: "idle",
      lastReconcileResult: "ok",
      lastReconcileError: null,
      updatedAt: THIRTY_DAYS_AGO,
      behaviorExecutorCapacity: 4,
      behaviorExecutorQueueDepth: 0,
    },
    principalConfig: {
      agent_did: AGENT_DID,
      display_name: "Bombadil UI Agent",
      default_behavior_id: DEFAULT_BEHAVIOR_ID,
      enabled: true,
      created_at: STARTED_AT,
      created_by: "bombadil",
    },
    behaviorConfigs: [
      {
        behavior_id: DEFAULT_BEHAVIOR_ID,
        agent_did: AGENT_DID,
        display_name: "Default",
        context_id: "context-default",
        inference_profile_id: "profile-default",
        enabled: true,
        tags: [],
        created_at: STARTED_AT,
      },
      {
        behavior_id: "ops",
        agent_did: AGENT_DID,
        display_name: "Ops",
        context_id: "context-ops",
        inference_profile_id: "profile-default",
        enabled: true,
        tags: [],
        created_at: STARTED_AT,
      },
    ],
    contexts: [
      {
        context_id: "context-default",
        agent_did: AGENT_DID,
        display_name: "Default context",
        system_prompt: "You are a deterministic UI-test agent.",
        tools_id: "tools-default",
        compaction_id: null,
        skill_ids: ["host-diagnostics"],
      },
      {
        context_id: "context-ops",
        agent_did: AGENT_DID,
        display_name: "Ops context",
        system_prompt: "You inspect runtime and fleet health.",
        tools_id: "tools-default",
        compaction_id: null,
        skill_ids: [],
      },
    ],
    compactions: [],
    behaviors: [
      {
        behaviorId: DEFAULT_BEHAVIOR_ID,
        agentDid: AGENT_DID,
        displayName: "Default",
        description: null,
        contextId: "context-default",
        inferenceProfileId: "profile-default",
        enabled: true,
        isDefault: true,
        tags: [],
        createdAt: STARTED_AT,
      },
      {
        behaviorId: "ops",
        agentDid: AGENT_DID,
        displayName: "Ops",
        description: null,
        contextId: "context-ops",
        inferenceProfileId: "profile-default",
        enabled: true,
        isDefault: false,
        tags: [],
        createdAt: STARTED_AT,
      },
    ],
    behaviorEnvironments: [
      {
        behaviorId: DEFAULT_BEHAVIOR_ID,
        displayName: "Default",
        enabled: true,
        isDefault: true,
        modelName: "gpt-4.1-mini",
        inferenceProfileName: "Default profile",
        workspaceRoot: "/tmp/gents-bombadil/workspace",
        fileAccess: "read-only",
        bashAccess: "read-only",
        networkAccess: "disabled",
        skillNames: ["Host diagnostics"],
        sessionCount: 0,
        activeSessionCount: 0,
      },
      {
        behaviorId: "ops",
        displayName: "Ops",
        enabled: true,
        isDefault: false,
        modelName: "gpt-4.1-mini",
        inferenceProfileName: "Default profile",
        workspaceRoot: "/tmp/gents-bombadil/workspace",
        fileAccess: "read-only",
        bashAccess: "read-only",
        networkAccess: "disabled",
        skillNames: [],
        sessionCount: 0,
        activeSessionCount: 0,
      },
    ],
    inferenceBackends: [
      {
        backendId: "backend-openai",
        name: "OpenAI Harness",
        providerKind: "OpenAiCompatible",
        endpoint: "http://127.0.0.1:8000/v1",
        apiKeyConfigured: true,
        apiKeyEnvVar: "OPENAI_API_KEY",
        authKind: "environment",
        openaiWireApi: null,
        connectTimeoutSecs: null,
        discoveryTimeoutSecs: null,
        maxConcurrent: 4,
        maxQueueDepth: 16,
        enabled: true,
        models: ["gpt-4.1-mini"],
        probeStatus: "healthy",
      },
    ],
    inferenceProfiles: [
      {
        agent_did: AGENT_DID,
        profile_id: "profile-default",
        display_name: "Default profile",
        backend_id: "backend-openai",
        model_name: "gpt-4.1-mini",
        context_window: 128000,
        max_output_tokens: 4096,
        sampling_id: null,
        execution_id: null,
      },
    ],
    tools: [
      {
        tools_id: "tools-default",
        agent_did: AGENT_DID,
        display_name: "Default tools",
        host: {
          root: "/tmp/gents-bombadil/workspace",
          files: { mode: "ReadOnly" },
          bash: { mode: "ReadOnly" },
          cli: [{ name: "rg" }, { name: "git" }],
        },
        remote: {
          services: [
            {
              mcp_service_id: "mcp-observability",
              tool_names: ["inspect_host", "query_logs", "fleet_status"],
              style: "discovery",
            },
          ],
        },
        built_ins: {
          enable_goal_tools: false,
          enable_goal_creation: false,
        },
      },
    ],
    toolServiceRegistries: [
      {
        service_id: "mcp-observability",
        agent_did: AGENT_DID,
        display_name: "Observability MCP",
        description: "Fleet health MCP service",
        hostname: "localhost",
        tailscale_ip: null,
        lan_ip: "127.0.0.1",
        mcp_port: 7331,
        mcp_path: "/mcp",
        enabled: true,
      },
    ],
    skills: [
      {
        skillId: "host-diagnostics",
        agentDid: AGENT_DID,
        name: "Host diagnostics",
        description: "Inspect host health and write a concise operational report.",
        instructions: "Inspect host health, telemetry freshness, and recent errors.",
        toolRefs: ["mcp-observability.inspect_host", "mcp-observability.query_logs"],
        displayName: "Host diagnostics",
        enabled: true,
        createdAt: STARTED_AT,
      },
      {
        skillId: "fleet-summary",
        agentDid: AGENT_DID,
        name: "Fleet summary",
        description: "Summarize fleet state for operator handoff.",
        instructions:
          "Compare backend health, silent hosts, and posted steward status.",
        toolRefs: ["mcp-observability.fleet_status"],
        displayName: "Fleet summary",
        enabled: true,
        createdAt: STARTED_AT,
      },
    ],
    tasks: [
      {
        taskId: "host-check",
        name: "Host check",
        description: "Inspect host health and summarize findings.",
        behaviorId: DEFAULT_BEHAVIOR_ID,
        promptTemplate: "Inspect this host and report health.",
        goalObjectiveTemplate: null,
        goalTokenBudget: null,
        enabled: true,
        outputSchemaRef: null,
        hooks: [],
        tags: [],
        recentRuns: {
          totalFires: 0,
          lastAttemptAt: null,
          lastStatus: null,
          lastError: null,
          scheduleCount: 1,
          eventCount: 0,
        },
        runHistory: [],
      },
    ],
    schedules: [
      {
        agent_did: AGENT_DID,
        schedule_id: "host-check-every-6h",
        display_name: "Host check every 6h",
        cadence: { kind: "interval", interval_secs: 21600 },
        tags: [],
      },
    ],
    eventSources: [],
    triggers: [],
    inferenceSampling: [],
    inferenceExecution: [],
    subagentTargets: [],
    datastoreToolSurfaces: [],
    chainKeyBindings: [],
    mailboxItems: [],
    sessions: [],
  };
}

function upsertBy<T extends Record<K, string>, K extends keyof T>(
  rows: T[],
  key: K,
  value: string,
  next: T,
) {
  const index = rows.findIndex((row) => row[key] === value);
  if (index < 0) {
    return [...rows, next];
  }
  return rows.map((row, i) => (i === index ? next : row));
}

function initialSyncHealth(scenario: DesktopUiHarnessScenario): SyncHealthView {
  if (scenario === "sync-offline") {
    return {
      state: "offline",
      lastError: "fixture transport unavailable",
      connectedPeerCount: 0,
      pendingDagCount: 0,
      persistedPendingDagCount: 0,
      pushRetryMarkerCount: 0,
      exhaustedFetchCount: 0,
      quarantinedDagCount: 0,
    };
  }
  if (scenario === "sync-failed") {
    return {
      state: "failed",
      lastError: "database DAG quarantined",
      connectedPeerCount: 1,
      pendingDagCount: 1,
      persistedPendingDagCount: 1,
      pushRetryMarkerCount: 0,
      exhaustedFetchCount: 1,
      quarantinedDagCount: 1,
    };
  }
  if (scenario === "session-hydration") {
    return {
      state: "healthy",
      lastError: null,
      connectedPeerCount: 1,
      pendingDagCount: 0,
      persistedPendingDagCount: 0,
      pushRetryMarkerCount: 0,
      exhaustedFetchCount: 0,
      quarantinedDagCount: 0,
    };
  }
  return {
    state: "healthy",
    lastError: null,
    connectedPeerCount: 1,
    pendingDagCount: 0,
    persistedPendingDagCount: 0,
    pushRetryMarkerCount: 0,
    exhaustedFetchCount: 0,
    quarantinedDagCount: 0,
  };
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

function normalizeScenario(value?: string | null): DesktopUiHarnessScenario {
  switch (value) {
    case "empty-fleet":
    case "loading":
    case "bridge-unavailable":
    case "save-error":
    case "backend-health-error":
    case "backend-unavailable":
    case "mailbox-overflow":
    case "long-content":
    case "active-turn":
    case "cascade-turn":
    case "coding":
    case "mobile-performance":
    case "session-hydration":
    case "sync-offline":
    case "sync-failed":
      return value;
    default:
      return "default";
  }
}

function longHarnessMessage() {
  return [
    "I am your desktop UI test agent with a deliberately long transcript row.",
    "This content exercises wrapping, scrolling, markdown layout, and composer stability without relying on a real model.",
    "The message keeps going so deterministic browser tests can capture a stable long-content chat state.",
    "Observation: fleet healthy, backend available, MCP service reachable, no duplicate assistant rows expected.",
  ].join("\n\n");
}

function wait(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}
