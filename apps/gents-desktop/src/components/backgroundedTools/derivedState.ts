import type {
  BackgroundedToolView,
  NativeExecutorStatusView,
} from "../../lib/types/operations";

export const STUCK_DWELL_MS = 5_000;
const CORRELATION_WINDOW_MS = 1_000;

export type DerivedState =
  "running" | "background" | "stuck" | "cancelPending" | "deadline+";

export function derivedState(row: BackgroundedToolView, nowMs: number): DerivedState {
  const stuckDwellMs = row.stuckSince
    ? nowMs - new Date(row.stuckSince).getTime()
    : null;
  if (stuckDwellMs != null && stuckDwellMs >= STUCK_DWELL_MS) return "stuck";
  if (row.cancelPendingRemoteAck) return "cancelPending";
  if (row.deadlineExpired) return "deadline+";
  if (row.awaitMode === "background") return "background";
  return (row.lifecycleState as DerivedState | null) ?? "running";
}

export type ProcessLabel = {
  label: string;
  tooltip: string;
};

export function correlateProcess(
  row: BackgroundedToolView,
  executors: NativeExecutorStatusView[],
): ProcessLabel {
  const startedAtIso = row.startedAt;
  if (startedAtIso) {
    const start = new Date(startedAtIso).getTime();
    const candidates = executors.filter(
      (ne) =>
        ne.toolName === row.toolName &&
        Math.abs(new Date(ne.startedAt).getTime() - start) <= CORRELATION_WINDOW_MS,
    );
    if (candidates.length === 1) {
      const ne = candidates[0];
      return { label: `pid ${ne.pid}`, tooltip: `native ${ne.id} · ${ne.argv0}` };
    }
    if (candidates.length > 1) {
      const c0 = candidates[0];
      return {
        label: `native ${c0.id}`,
        tooltip: `ambiguous: ${candidates.length} candidates — ${candidates
          .map((c) => `native ${c.id}/pid ${c.pid}`)
          .join(", ")}`,
      };
    }
  }
  if (row.childRequestId) {
    return { label: `child ${row.childRequestId}`, tooltip: "subagent dispatch" };
  }
  return { label: "—", tooltip: "no native executor; in-process tool" };
}

export function formatAge(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${pad(h)}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}
