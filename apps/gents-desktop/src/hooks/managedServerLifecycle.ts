import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

const managedServerRestoreInFlight = new WeakMap<
  DesktopApiAdapter,
  Promise<boolean | null>
>();

export function restoreManagedServer(api: DesktopApiAdapter): Promise<boolean | null> {
  const existing = managedServerRestoreInFlight.get(api);
  if (existing) return existing;

  const pending = restoreManagedServerOnce(api).finally(() => {
    if (managedServerRestoreInFlight.get(api) === pending) {
      managedServerRestoreInFlight.delete(api);
    }
  });
  managedServerRestoreInFlight.set(api, pending);
  return pending;
}

async function restoreManagedServerOnce(
  api: DesktopApiAdapter,
): Promise<boolean | null> {
  if (!api.managedServerStatus || !api.startManagedServer) return null;

  const status = await api.managedServerStatus();
  if (status.state === "running" || status.state === "external") {
    return true;
  }
  if (!status.autoStart) {
    return false;
  }

  await api.startManagedServer(status.agentName?.trim() || "Local Agent");
  return true;
}
