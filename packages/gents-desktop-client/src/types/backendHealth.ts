import type { BackendHealthView } from "../generated/BackendHealthView.js";
import type { InferenceCallSummaryView } from "../generated/InferenceCallSummaryView.js";

export type BackendDisplayState =
  | "available"
  | "unhealthy"
  | "stale"
  | "rate-limited"
  | "circuit-open"
  | "unknown"
  | "disabled";

export type InferenceCallSummary = InferenceCallSummaryView;

export type BackendHealth = Omit<BackendHealthView, "displayState"> & {
  displayState: BackendDisplayState;
};
