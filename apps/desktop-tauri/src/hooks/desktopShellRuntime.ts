import type { ChatWorkflowState } from "../lib/chat-shell";
import type { P2PHealth } from "../lib/types";

export type DesktopShellTimingConfig = {
  p2pAutoRestartCooldownMs: number;
  clientRestartMaxAttempts: number;
  clientRestartBackoffMs: number;
};

const DEFAULT_TIMING_CONFIG: DesktopShellTimingConfig = {
  p2pAutoRestartCooldownMs: 20_000,
  clientRestartMaxAttempts: 10,
  clientRestartBackoffMs: 250,
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

  if (
    workflow.kind === "awaitingObservation" ||
    workflow.kind === "turnInProgress"
  ) {
    return workflow.sessionId === sessionId ? workflow.requestId ?? null : null;
  }

  return null;
}
