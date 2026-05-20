/**
 * Mirrors `BackendHealthView` / `InferenceCallSummaryView` from
 * `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs`.
 * The Tauri layer serializes with `#[serde(rename_all = "camelCase")]`,
 * so field names here are camelCase even though the underlying
 * `InferenceBackend` / `InferenceCall` columns are snake_case.
 */
export type BackendDisplayState =
  | "available"
  | "unhealthy"
  | "stale"
  | "rate-limited"
  | "circuit-open"
  | "unknown"
  | "disabled";

export type InferenceCallSummary = {
  callId: string;
  callSeq: number;
  callKind: string;
  callState: string;
  failureReason: string | null;
  queuedAt: string | null;
  startedAt: string | null;
  endedAt: string | null;
  queueDepthAtEnqueue: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
};

export type BackendHealth = {
  backendId: string;
  name: string;
  providerKind: string;
  endpoint: string;
  enabled: boolean;
  probeStatus: string;
  displayState: BackendDisplayState;
  lastProbe: string | null;
  maxConcurrent: number;
  maxQueueDepth: number;
  models: string[];
  recentCalls: InferenceCallSummary[];
};
