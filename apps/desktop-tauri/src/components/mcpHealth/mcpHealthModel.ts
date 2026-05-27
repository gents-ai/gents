import type { MCPServiceHealthView } from "../../lib/types";

export type FilterId = "all" | "unhealthy" | "reconnecting";

export type VisualState =
  | "healthy"
  | "degraded"
  | "evicted"
  | "reconnecting"
  | "stuck"
  | "unknown";

export function visualState(service: MCPServiceHealthView): VisualState {
  // Accept both the persisted vocabulary ("degraded") and the public
  // HealthStatus name ("stale") so the panel keeps working if any older
  // row was written before the schema/vocab alignment landed.
  const raw = (service.status ?? "").toLowerCase();
  const status = raw === "stale" ? "degraded" : raw;
  const failureCount = service.failureCount ?? 0;
  const kMax = service.kMax ?? 1;
  // Derived "stuck" badge — failure_count >= 2K or last_seen older than 5 min.
  // The runtime does not model `Stuck`; the panel surfaces it visually so
  // operators have a clear "investigate now" signal without changing the
  // K-model state machine.
  const stuck =
    (status === "evicted" || status === "reconnecting") &&
    (failureCount >= 2 * Math.max(1, kMax) || isLastSeenOlderThan(service, 5 * 60_000));
  if (stuck) return "stuck";
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

export function projectStatus(
  visual: VisualState,
): "healthy" | "stale" | "unreachable" | "unknown" {
  if (visual === "healthy") return "healthy";
  if (visual === "degraded") return "stale";
  if (visual === "evicted" || visual === "reconnecting" || visual === "stuck") {
    return "unreachable";
  }
  return "unknown";
}

export function statusLabel(visual: VisualState): string {
  switch (visual) {
    case "healthy":
      return "healthy";
    case "degraded":
      return "degraded";
    case "evicted":
      return "evicted (backoff)";
    case "reconnecting":
      return "reconnecting";
    case "stuck":
      return "stuck";
    default:
      return "unknown";
  }
}

export function matchesFilter(
  service: MCPServiceHealthView,
  filter: FilterId,
): boolean {
  if (filter === "all") return true;
  const visual = visualState(service);
  if (filter === "unhealthy") return projectStatus(visual) !== "healthy";
  if (filter === "reconnecting") return visual === "reconnecting";
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

function isLastSeenOlderThan(service: MCPServiceHealthView, ms: number): boolean {
  if (!service.lastSeen) return false;
  const ts = Date.parse(service.lastSeen);
  if (Number.isNaN(ts)) return false;
  return Date.now() - ts > ms;
}

function formatRemaining(ms: number): string {
  const totalSec = Math.max(0, Math.ceil(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  if (totalSec < 5) return "imminent";
  return `${m}:${String(s).padStart(2, "0")}`;
}
