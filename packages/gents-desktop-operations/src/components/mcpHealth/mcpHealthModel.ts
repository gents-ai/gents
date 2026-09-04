import type { MCPServiceHealthView } from "@source-inc/gents-desktop-client";

export type FilterId = "all" | "unhealthy" | "reconnecting";

export type VisualState =
  "healthy" | "degraded" | "evicted" | "reconnecting" | "unknown";

export type DisplayProjection = "healthy" | "stale" | "unreachable" | "unknown";

// Raw persisted `status` typed for presentation (dot color, CSS class).
// This mirrors the server's raw vocabulary verbatim — it does not
// classify or invent any state. The single owner of MCP health
// classification is `MCPServiceHealthView.displayState`
// (`ToolServiceHealthState::project` in Rust); see `projectStatus`.
export function visualState(service: MCPServiceHealthView): VisualState {
  const raw = (service.status ?? "").toLowerCase();
  const status = raw === "stale" ? "degraded" : raw;
  switch (status) {
    case "healthy":
      return "healthy";
    case "degraded":
      return "degraded";
    case "evicted":
      return "evicted";
    case "reconnecting":
      return "reconnecting";
    default:
      return "unknown";
  }
}

// Pass-through of the server-projected `displayState`. This is the only
// classification the desktop performs — no TS heuristic re-derives it
// from `status`/`failureCount`/`lastSeen`. The `"unknown"` fallback is
// the single place a missing/unrecognized `displayState` is handled.
export function projectStatus(service: MCPServiceHealthView): DisplayProjection {
  const displayState = service.displayState;
  if (
    displayState === "healthy" ||
    displayState === "stale" ||
    displayState === "unreachable"
  ) {
    return displayState;
  }
  return "unknown";
}

export function statusLabel(projection: DisplayProjection): string {
  switch (projection) {
    case "healthy":
      return "healthy";
    case "stale":
      return "stale";
    case "unreachable":
      return "unreachable";
    default:
      return "unknown";
  }
}

export function matchesFilter(
  service: MCPServiceHealthView,
  filter: FilterId,
): boolean {
  if (filter === "all") return true;
  if (filter === "unhealthy") return projectStatus(service) !== "healthy";
  if (filter === "reconnecting") return visualState(service) === "reconnecting";
  return true;
}

export function formatRelative(timestamp: string | null | undefined): string {
  if (!timestamp) return "never";
  const ts = Date.parse(timestamp);
  if (Number.isNaN(ts)) return timestamp;
  const ageMs = Date.now() - ts;
  if (ageMs < 0) {
    return formatRemaining(-ageMs).startsWith("imminent")
      ? "imminent"
      : `in ${formatRemaining(-ageMs)}`;
  }
  const s = Math.floor(ageMs / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

export function backoffRemaining(backoffUntil: string | null | undefined): {
  remainingMs: number;
  text: string;
} | null {
  if (!backoffUntil) return null;
  const target = Date.parse(backoffUntil);
  if (Number.isNaN(target)) return null;
  const remainingMs = target - Date.now();
  if (remainingMs <= 0) return { remainingMs: 0, text: "backoff expired" };
  return { remainingMs, text: `retry in ${formatRemaining(remainingMs)}` };
}

function formatRemaining(ms: number): string {
  const totalSec = Math.max(0, Math.ceil(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  if (totalSec < 5) return "imminent";
  return `${m}:${String(s).padStart(2, "0")}`;
}

export type McpProbeOutcome = {
  at: string;
  status?: string;
  latencyMs?: number;
  lastError?: string | null;
  error?: string;
};
