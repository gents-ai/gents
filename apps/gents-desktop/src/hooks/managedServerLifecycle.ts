import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import {
  awaitManagedServerSettled,
  ManagedServerStartupError,
  observeManagedServerOperation,
  unsettledManagedServerError,
  type ManagedServerWait,
} from "../lib/managedServerStartup";

type RestoreOptions = {
  onWait?: (wait: ManagedServerWait | null) => void;
  signal?: AbortSignal;
};

const managedServerRestoreInFlight = new WeakMap<
  DesktopApiAdapter,
  Promise<boolean | null>
>();

/**
 * Observes the OS-owned service at launch. Concurrent callers share one
 * observation, so only the first caller's `onWait` and `signal` apply.
 * Rejects with `ManagedServerStartupError` when the service stays booting or
 * blocked past the bridge's bounds, or reports a failure.
 */
export function restoreManagedServer(
  api: DesktopApiAdapter,
  options: RestoreOptions = {},
): Promise<boolean | null> {
  const existing = managedServerRestoreInFlight.get(api);
  if (existing) return existing;

  const pending = restoreManagedServerOnce(api, options).finally(() => {
    if (managedServerRestoreInFlight.get(api) === pending) {
      managedServerRestoreInFlight.delete(api);
    }
  });
  managedServerRestoreInFlight.set(api, pending);
  return pending;
}

async function restoreManagedServerOnce(
  api: DesktopApiAdapter,
  { onWait = () => {}, signal }: RestoreOptions,
): Promise<boolean | null> {
  if (!api.managedServerStatus) return null;

  let awaitedApproval = false;
  const status = await awaitManagedServerSettled(
    api,
    await api.managedServerStatus(),
    (wait) => {
      if (wait?.kind === "approval") awaitedApproval = true;
      onWait(wait);
    },
    { signal },
  );
  if (!signal?.aborted) {
    const failure = unsettledManagedServerError(status);
    if (failure) throw failure;
  }
  if (status.state === "running" || status.state === "external") {
    return true;
  }
  // macOS kept an agent enabled at login from launching until the user
  // allowed it here, so start it as launchd would have, with its stored
  // authority. A deliberately stopped agent stays stopped.
  const agentName = status.agentName?.trim();
  const startManagedServer = api.startManagedServer;
  if (
    awaitedApproval &&
    status.autoStart &&
    !signal?.aborted &&
    !status.approvalRequired &&
    agentName &&
    startManagedServer &&
    (status.state === "stopped" || status.state === "disabled")
  ) {
    try {
      const started = await observeManagedServerOperation(
        api,
        () => startManagedServer(agentName),
        onWait,
      );
      return started.state === "running" || started.state === "external";
    } catch (cause) {
      throw new ManagedServerStartupError(
        cause instanceof Error ? cause.message : String(cause),
        status,
      );
    }
  }
  // Native launchd/systemd ownership is intentionally independent of the GUI.
  // A stopped enabled service is an OS observation, not permission for this
  // frontend to create a second process during application startup.
  return false;
}
