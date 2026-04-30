export type RunnerReadyMessage = {
  kind: "ready";
  baseUrl: string;
  deploymentLabel: string;
  agentDid: string;
  toolRoot: string;
};

export type VersionResponse = {
  version: number;
};

type ToolCallDiagnostics = {
  total: number;
  completed: number;
  pending: number;
  latestToolName?: string | null;
  latestStatus?: string | null;
  latestCompletedAt?: string | null;
};

export type RequestDiagnostics = {
  source: string;
  sessionId: string;
  requestId: string;
  turnState?: string | null;
  latestRequestId?: string | null;
  conversationUpdatedAt?: string | null;
  request?: {
    status?: string | null;
    lifecycleState?: string | null;
    failureReason?: string | null;
    createdAt?: string | null;
    claimedAt?: string | null;
    interruptRequestedAt?: string | null;
    validUntil?: string | null;
  } | null;
  response?: {
    status?: string | null;
    errorMessage?: string | null;
    progressSeq?: number | null;
    materializedMessageSequence?: number | null;
    materializedAt?: string | null;
    completedAt?: string | null;
    contentLen: number;
    reasoningLen: number;
  } | null;
  toolCalls: ToolCallDiagnostics;
  toolResultCount: number;
  messageCount: number;
  timelineCount: number;
  activeResponseOverlayContentLen: number;
  activeResponseOverlayReasoningLen: number;
};

export type RequestDiagnosticsBundle = {
  desktop: RequestDiagnostics;
  remote: RequestDiagnostics;
};

export type RemoteTerminalDesktopStallObservation = {
  startedAt: number | null;
  stallMs: number | null;
  exceededThreshold: boolean;
};

export type RemoteAheadDesktopLagObservation = {
  startedAt: number | null;
  lagMs: number | null;
  exceededThreshold: boolean;
};

export type LiveBridgeRunnerOptions = {
  inferenceUrl?: string | null;
  modelName?: string | null;
  provider?: string | null;
  apiKey?: string | null;
  apiKeyEnvVar?: string | null;
};
