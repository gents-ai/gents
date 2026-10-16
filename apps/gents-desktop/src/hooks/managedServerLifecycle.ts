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
  // macOS kept a node enabled at login from launching until the user
  // allowed it here, so start it as launchd would have, with its stored
  // authority. A deliberately stopped node stays stopped.
  const nodeName = status.nodeName?.trim();
  const startManagedServer = api.startManagedServer;
  if (
    status.autoStart &&
    !signal?.aborted &&
    !status.approvalRequired &&
    nodeName &&
    startManagedServer &&
    (status.state === "stopped" || status.state === "disabled")
  ) {
    try {
      const started = await observeManagedServerOperation(
        api,
        () => startManagedServer(nodeName),
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
  return false;
}
