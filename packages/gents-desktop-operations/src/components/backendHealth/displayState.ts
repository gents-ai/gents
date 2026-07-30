import type { BackendDisplayState } from "@source-inc/gents-desktop-client";

export function deriveDisplayState(
  enabled: boolean,
  probeStatus: string,
): BackendDisplayState {
  if (!enabled) return "disabled";
  switch (probeStatus) {
    case "healthy":
      return "available";
    case "unhealthy":
      return "unhealthy";
    case "stale":
      return "stale";
    case "rate_limited":
      return "rate-limited";
    case "circuit_open":
      return "circuit-open";
    case "unknown":
    default:
      return "unknown";
  }
}

export const STATE_LABEL: Record<BackendDisplayState, string> = {
  available: "Available",
  unhealthy: "Unhealthy",
  stale: "Stale",
  "rate-limited": "Rate-limited",
  "circuit-open": "Circuit-open",
  unknown: "Unknown",
  disabled: "Disabled",
};

export const STATE_GLYPH: Record<BackendDisplayState, string> = {
  available: "●",
  unhealthy: "×",
  stale: "!",
  "rate-limited": "⏱",
  "circuit-open": "⊘",
  unknown: "?",
  disabled: "—",
};
