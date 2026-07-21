import type { BackendDisplayState } from "./types";

/**
 * JS mirror of `gents::backend_registry::derive_display_state`.
 * Kept in sync by the bridge-snapshot consumer test in gents
 * (`backend_registry::tests::display_state_matches_every_lean_backend_health_admission_case`)
 * and by the component test in `tests/backend-health-panel.test.tsx`,
 * which enumerates the same Lean witness inputs and asserts this JS
 * function produces the same bucket the Rust function does.
 *
 * The bridge already attaches `displayState` to every
 * `BackendHealthView` it returns, so most consumers should read
 * `backend.displayState` directly. This helper exists so the test
 * harness can drive the mapping in isolation.
 */
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
