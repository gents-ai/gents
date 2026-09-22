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
  if (!api.managedServerStatus) return null;

  const status = await api.managedServerStatus();
  if (status.state === "running" || status.state === "external") {
    return true;
  }
  // Native launchd/systemd ownership is intentionally independent of the GUI.
  // A stopped enabled service is an OS observation, not permission for this
  // frontend to create a second process during application startup.
  return false;
}
