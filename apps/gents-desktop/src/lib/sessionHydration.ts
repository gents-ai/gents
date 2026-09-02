import type { SessionHydrationView } from "@source-inc/gents-desktop-client";

export type VisibleHydrationPhase = "requested" | "serving" | "complete" | "failed";

export type VisibleSessionHydration = SessionHydrationView & {
  phase: VisibleHydrationPhase;
};

export function visibleSessionHydration(
  hydration: SessionHydrationView | null | undefined,
  sessionId: string | null,
  agentDid?: string | null,
): VisibleSessionHydration | null {
  if (!hydration || !sessionId || hydration.sessionId !== sessionId) {
    return null;
  }
  if (agentDid && hydration.agentDid !== agentDid) {
    return null;
  }
  if (
    hydration.phase !== "requested" &&
    hydration.phase !== "serving" &&
    hydration.phase !== "complete" &&
    hydration.phase !== "failed"
  ) {
    return null;
  }
  if (
    hydration.phase === "complete" &&
    (hydration.servedCount ?? hydration.mergedCount) === 0
  ) {
    return null;
  }
  return hydration as VisibleSessionHydration;
}

export function sessionHydrationLabel(hydration: VisibleSessionHydration): string {
  const served = hydration.servedCount;
  const covered = hydration.coveredCount;
  switch (hydration.phase) {
    case "requested":
      return "Fetching session history";
    case "serving":
      return served == null
        ? covered > 0
          ? `Fetching session history · ${covered} documents so far`
          : "Fetching session history"
        : `Fetching session history · ${covered} of ${served}`;
    case "complete":
      return served == null
        ? "Session history loaded"
        : `Session history loaded · ${covered} of ${served}`;
    case "failed":
      return "Couldn't fetch the rest of this session";
  }
}

export function sessionHydrationNeedsRetry(
  hydration: VisibleSessionHydration,
): boolean {
  return hydration.phase === "failed";
}
