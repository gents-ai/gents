import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { ManagedServerRepairResult } from "@source-inc/gents-desktop-client/generated/ManagedServerRepairResult";

import {
  awaitManagedServerSettled,
  ManagedServerStartupError,
  observeManagedServerOperation,
  unsettledManagedServerError,
  type ManagedServerWait,
} from "../lib/managedServerStartup";
import { errorText } from "../lib/errorText";

type RestoreOptions = {
  onWait?: (wait: ManagedServerWait | null) => void;
  signal?: AbortSignal;
  onRepair?: (result: ManagedServerRepairResult) => void;
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
  { onWait = () => {}, signal, onRepair }: RestoreOptions,
): Promise<boolean | null> {
  if (!api.managedServerStatus) return null;

  const repair = await api.repairManagedServer?.();
  if (repair) {
    onRepair?.(repair);
    if (repair.outcome === "blocked") {
      throw new ManagedServerStartupError(
        repair.blocker ?? "Gents could not finish starting.",
        await api.managedServerStatus(),
      );
    }
    if (repair.outcome === "needsSetup") return false;
  }

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
    if (failure) {
      // The repair sets aside a home its node refused as an older
      // version's data, which it can see only once the node has refused it.
      const retried =
        failure.status.errorCode === "incompatibleLocalStore"
          ? await api.repairManagedServer?.()
          : undefined;
      if (retried?.outcome === "needsSetup") {
        onRepair?.({
          ...retried,
          repaired: [...(repair?.repaired ?? []), ...retried.repaired],
        });
        return false;
      }
      throw failure;
    }
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
    awaitedApproval &&
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
      throw new ManagedServerStartupError(errorText(cause), status);
    }
  }
  // The GUI repairs through launchd under the same label and never creates a
  // second process; a stopped enabled service is not permission to start one.
  return false;
}
