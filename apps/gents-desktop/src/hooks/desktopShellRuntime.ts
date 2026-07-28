import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type { P2PHealth } from "@source-inc/gents-desktop-client";

export type DesktopShellTimingConfig = {
  p2pAutoRestartCooldownMs: number;
  clientRestartMaxAttempts: number;
  clientRestartBackoffMs: number;
  activeSessionPollMs: number;
};

const DEFAULT_TIMING_CONFIG: DesktopShellTimingConfig = {
  p2pAutoRestartCooldownMs: 20_000,
  clientRestartMaxAttempts: 10,
  clientRestartBackoffMs: 250,
  activeSessionPollMs: 1_500,
};

let timingConfigOverrides: Partial<DesktopShellTimingConfig> | null = null;

export function timingConfig(): DesktopShellTimingConfig {
  return {
    ...DEFAULT_TIMING_CONFIG,
    ...timingConfigOverrides,
  };
}

export function setDesktopShellTimingConfigForTests(
  overrides: Partial<DesktopShellTimingConfig> | null,
) {
  timingConfigOverrides = overrides;
}

export function shouldAutoRestartP2P(
  previous: P2PHealth | null,
  next: P2PHealth | null,
  lastAttemptAt: number | null,
  now: number,
  cooldownMs: number,
) {
  if (!next || next.status !== "wedged") {
    return false;
  }

  if (lastAttemptAt !== null && now - lastAttemptAt < cooldownMs) {
    return false;
  }

  if (!previous) {
    return true;
  }

  return (
    previous.status !== "wedged" ||
    previous.consecutiveFailures !== next.consecutiveFailures ||
    previous.lastError !== next.lastError
  );
}

export async function delay(ms: number) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

export type TrailingRefreshQueue = {
  request: () => Promise<void>;
  dispose: () => void;
};

/**
 * Runs at most one refresh at a time and retains at most one trailing pass.
 *
 * DefraDB can publish many store versions while a replicated subagent batch is
 * landing. Rendering each version would start overlapping full snapshot
 * queries; the trailing pass guarantees the UI eventually observes the newest
 * version without replaying every intermediate one.
 */
export function createTrailingRefreshQueue(
  refresh: () => Promise<void>,
): TrailingRefreshQueue {
  let active: Promise<void> | null = null;
  let disposed = false;
  let pending = false;

  const drain = async () => {
    do {
      pending = false;
      try {
        await refresh();
      } catch (error) {
        logShellEvent(`coalesced refresh failed error=${String(error)}`);
      }
    } while (!disposed && pending);
  };

  return {
    request() {
      if (disposed) {
        return Promise.resolve();
      }

      pending = true;
      if (!active) {
        active = drain().finally(() => {
          active = null;
        });
      }
      return active;
    },
    dispose() {
      disposed = true;
      pending = false;
    },
  };
}

export function logShellEvent(message: string) {
  console.info(`[live-tauri-shell] ${message}`);
}

export function trackedRequestIdForSession(
  sessionId: string | null,
  workflow: ChatWorkflowState,
) {
  if (!sessionId) {
    return null;
  }

  if (workflow.kind === "awaitingObservation" || workflow.kind === "turnInProgress") {
    return workflow.sessionId === sessionId ? (workflow.requestId ?? null) : null;
  }

  return null;
}
