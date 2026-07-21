// 1:1 mirror of bridge/types/views/operations.rs. Keep these in sync.
// Panels in their own PRs import from "../lib/types" (re-exported below).

export type DesktopOperationsSnapshot = {
  fetchedAt: string;
  agentDid?: string | null;
  liveness?: RuntimeLivenessView | null;
  livenessUnavailableReason?: string | null;
  backgroundedTools: BackgroundedToolView[];
  stuckDiagnostics: StuckWorkDiagnosticView[];
  lineage?: SubagentTreeView | null;
};

export type RuntimeLivenessView = {
  expiredProcessingCount: number;
  requests: ActiveRequestView[];
  activeToolCalls: ActiveToolCallView[];
  activeNativeExecutorsAvailable: boolean;
  activeNativeExecutors: NativeExecutorStatusView[];
};

export type ActiveRequestView = {
  requestId: string;
  claimedAt?: string | null;
  deadline?: string | null;
  deadlineExpired: boolean;
  deadlineAgeMs?: number | null;
  lastProgressAgeMs: number;
  subagentDepth: number;
  causedByParentRequestId?: string | null;
  causedByTriggerKind?: string | null;
};

export type ActiveToolCallView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  startedAt?: string | null;
  deadlineAt?: string | null;
  awaitMode?: string | null;
  runningAgeMs: number;
  deadlineExpired: boolean;
};

export type NativeExecutorStatusView = {
  id: number;
  pid: number;
  argv0: string;
  toolName?: string | null;
  startedAt: string;
  ageMs: number;
};

export type BackgroundedToolView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  lifecycleState?: string | null;
  status?: string | null;
  startedAt?: string | null;
  ageMs?: number | null;
  deadlineAt?: string | null;
  deadlineExpired: boolean;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
  childRequestId?: string | null;
  stuckSince?: string | null;
  cancelPendingRemoteAck: boolean;
  nativeExecutor?: NativeExecutorStatusView | null;
};

export type StuckWorkDiagnosticView = {
  requestId: string;
  sessionId?: string | null;
  severity: "warning" | "critical";
  reason: "expiredProcessing" | "expiredTool" | "stuckTool" | "pendingRemoteCancelAck";
  deadlineAgeMs?: number | null;
  lastProgressAgeMs?: number | null;
  toolCallId?: string | null;
  toolName?: string | null;
  stuckSince?: string | null;
};

export type SubagentTreeView = {
  rootRequestId: string;
  nodes: SubagentNodeView[];
  edges: SubagentEdgeView[];
  truncated: boolean;
  /** Deployments that could not be queried; their branches may be missing. */
  partialErrors?: string[];
};

export type SubagentNodeView = {
  requestId: string;
  /** Peer label the row was resolved from; absent = the local node. */
  resolvedVia?: string | null;
  sessionId?: string | null;
  agentDid?: string | null;
  behaviorId?: string | null;
  lifecycleState?: string | null;
  status?: string | null;
  subagentDepth?: number | null;
  causedByParentRequestId?: string | null;
  causedByParentToolCallId?: string | null;
  backendId?: string | null;
};

export type SubagentEdgeView = {
  parentRequestId: string;
  childRequestId: string;
  parentToolCallId?: string | null;
  toolName?: string | null;
  awaitMode?: "foreground" | "background" | string | null;
  cancelPolicy?: "cascade" | "detach" | string | null;
  lifecycleState?: string | null;
};

export type CascadeCancelPreview = {
  rootRequestId: string;
  previewSignature: string;
  rootState?: string | null;
  willInterrupt: CascadeAffectedRequest[];
  willDetach: CascadeAffectedRequest[];
  alreadyTerminal: CascadeAffectedRequest[];
  unknownPolicy: CascadeAffectedRequest[];
};

export type CascadeAffectedRequest = {
  requestId: string;
  sessionId?: string | null;
  behaviorId?: string | null;
  lifecycleState?: string | null;
  parentRequestId?: string | null;
  parentToolCallId?: string | null;
  toolName?: string | null;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
};

export type InterruptRequestResult = {
  requestId: string;
  accepted: boolean;
  interruptRequestedAt?: string | null;
  alreadyInterrupted: boolean;
  stalePreview: boolean;
  preview?: CascadeCancelPreview | null;
};

export type DerivedCancelCauseView = {
  cause: "userCancelled" | "interrupted" | "deadline" | "unknown";
  source:
    | "requestInterrupt"
    | "parentCascade"
    | "deadline"
    | "toolLifecycle"
    | "responseInterruptedAt"
    | "unresolved";
  confidence: "direct" | "derived";
  at?: string | null;
  evidence: string[];
};

// Command request shapes (mirror bridge/types/requests/operations.rs).

export type DesktopOperationsSnapshotRequest = {
  agentDid?: string | null;
  rootRequestId?: string | null;
  includeTerminal?: boolean;
};

export type DesktopListSubagentTreeRequest = {
  rootRequestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
  maxDepth?: number;
};

export type DesktopPreviewInterruptCascadeRequest = {
  requestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
};

export type DesktopInterruptRequestRequest = {
  requestId: string;
  cause: "userCancelled";
  cascade: boolean;
  expectedPreviewSignature?: string | null;
};

// MCP health panel (panel #278) — mirrors `MCPServiceHealthView` and
// `McpServiceProbeResult` in bridge/types/views/operations.rs. `status`
// is the precise `HealthState.toDefraDB` projection from
// Proofs/MCPHealth/State.lean so the panel can distinguish back-off
// (`evicted`) from in-flight retry (`reconnecting`) — the public
// three-state `HealthStatus` collapses both to `unreachable`.
export type MCPServiceHealthView = {
  serviceId: string;
  agentDid?: string | null;
  endpoint?: string | null;
  status?: "healthy" | "degraded" | "evicted" | "reconnecting" | string | null;
  failureCount?: number | null;
  kMax?: number | null;
  backoffUntil?: string | null;
  lastProbeAt?: string | null;
  lastSeen?: string | null;
  lastErrorClass?: string | null;
  lastErrorMessage?: string | null;
  updatedAt?: string | null;
};

export type McpServiceProbeResult = {
  serviceId: string;
  status: string;
  latencyMs: number;
  lastError?: string | null;
};

export type DesktopProbeMcpServiceRequest = {
  serviceId: string;
};

export type WorkspaceEntryView = {
  name: string;
  kind: "dir" | "file";
  size?: number | null;
};

export type WorkspaceListingView = {
  root: string;
  subpath: string;
  entries: WorkspaceEntryView[];
  truncated: boolean;
};

export type HeldToolCallView = {
  toolCallId: string;
  requestId: string | null;
  sessionId: string | null;
  agentDid: string | null;
  toolName: string | null;
  args: string | null;
  deadlineAt: string | null;
};

export type DesktopResolveHoldRequest = {
  agentDid: string;
  toolCallId: string;
  approve: boolean;
  reason?: string | null;
};

export type ResolveHoldResult = {
  approvalId: string;
  toolCallId: string;
  decision: string;
};
