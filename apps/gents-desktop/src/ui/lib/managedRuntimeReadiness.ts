import type {
  DesktopApiAdapter,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

type ReadinessApi = Pick<
  DesktopApiAdapter,
  "managedServerStatus" | "startManagedServer"
>;

export const MANAGED_RUNTIME_READY_TIMEOUT_MS = 30_000;

/** Raised when the managed runtime cannot be brought to a serving state. */
export class ManagedRuntimeUnavailableError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ManagedRuntimeUnavailableError";
  }
}

const sleep = (ms: number) =>
  new Promise<void>((resolve) => globalThis.setTimeout(resolve, ms));

/**
 * Polls the bridge's managed-server readiness observation until the runtime
 * reports secure pairing ready. Shared by first-run provisioning and setup
 * re-entry so both wait on the same owner.
 */
export async function waitForManagedRuntimePairing(
  api: ReadinessApi,
  { timeoutMs = MANAGED_RUNTIME_READY_TIMEOUT_MS, intervalMs = 250 } = {},
): Promise<ManagedServerStatus | null> {
  if (!api.managedServerStatus) return null;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const status = await api.managedServerStatus();
    if (status.pairingReady) return status;
    await sleep(intervalMs);
  }
  const status = await api.managedServerStatus();
  if (!status.pairingReady) {
    throw new ManagedRuntimeUnavailableError(
      "The local agent started, but secure background pairing is not ready.",
    );
  }
  return status;
}

/**
 * Makes sure the local managed runtime is serving before a setup step that
 * writes to it. Setup re-entry opens directly at the provider step, so it
 * reuses first run's owner: start the managed server with its stored
 * authority, then wait for readiness. Never starts a second lifecycle.
 */
export async function ensureManagedRuntimeServing(
  api: ReadinessApi,
  fallbackAgentName: string,
  options: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<void> {
  if (!api.managedServerStatus) return;
  let status: ManagedServerStatus;
  try {
    status = await api.managedServerStatus();
    if (status.pairingReady) return;
    // A runtime that is already starting or running is waited on, not
    // restarted; only a stopped or failed runtime is started again.
    const needsStart =
      status.state === "stopped" ||
      status.state === "failed" ||
      status.state === "disabled";
    if (needsStart) {
      if (!api.startManagedServer) {
        throw new ManagedRuntimeUnavailableError("The local agent is not running.");
      }
      await api.startManagedServer(
        status.agentName?.trim() || fallbackAgentName.trim() || "Local Agent",
      );
    }
    await waitForManagedRuntimePairing(api, options);
  } catch (cause) {
    console.warn("managed runtime is not serving for setup", cause);
    throw new ManagedRuntimeUnavailableError(
      "The local agent is not running, so provider sign-in is unavailable. Start the agent and try again.",
    );
  }
}
