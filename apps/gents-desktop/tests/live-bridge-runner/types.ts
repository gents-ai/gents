export type RunnerReadyMessage = {
  kind: "ready";
  baseUrl: string;
  deploymentLabel: string;
  nodeDid: string;
  toolRoot: string;
  dataRoot?: string;
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

type InferenceCallDiagnostics = {
  callId: string;
  requestId: string;
  requestDocId: string;
  nodeDid: string;
  backendId: string | null;
  agentId: string | null;
  callKind: string;
  callState: string;
};

export type RequestDiagnostics = {
  source: string;
  sessionId: string;
  requestId: string;
  turnState?: string | null;
  latestRequestId?: string | null;
  sessionUpdatedAt?: string | null;
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
    materializedMessageSequence?: number | null;
    materializedAt?: string | null;
    completedAt?: string | null;
    contentLen: number;
    reasoningLen: number;
  } | null;
  toolCalls: ToolCallDiagnostics;
  inferenceCalls?: InferenceCallDiagnostics[];
  inferenceDiagnosticsError?: string | null;
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
  agentTargetInferenceUrl?: string | null;
  agentTargetModelName?: string | null;
  agentTargetProvider?: string | null;
  agentTargetApiKey?: string | null;
  agentTargetApiKeyEnvVar?: string | null;
};
