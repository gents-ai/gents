import type { ManagedServerResetResult } from "../generated/ManagedServerResetResult.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, ManagedServerStatus } from "./types.js";

type ManagedServerCommand =
  | "managedServerStatus"
  | "startManagedServer"
  | "restartManagedServer"
  | "resetManagedServer"
  | "validateManagedServerRoot"
  | "openManagedServerLoginItems"
  | "commitManagedServerAutoStart"
  | "stopManagedServer"
  | "setManagedServerAutoStart";

export function createManagedServerCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, ManagedServerCommand> {
  return {
    managedServerStatus: () =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_status"),
    startManagedServer: (nodeName, authority) =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_start", {
        request: {
          nodeName,
          toolCeiling: authority?.toolCeiling ?? null,
          toolRoot: authority?.toolRoot ?? null,
        },
      }),
    restartManagedServer: (nodeName, authority) =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_restart", {
        request: { nodeName, ...authority },
      }),
    resetManagedServer: (confirmation, disposition) =>
      invokeDesktop<ManagedServerResetResult>("desktop_managed_server_reset", {
        request: {
          confirmation: confirmation ?? null,
          disposition: disposition ?? null,
        },
      }),
    validateManagedServerRoot: (path) =>
      invokeDesktop<{ canonicalPath: string }>(
        "desktop_managed_server_validate_root",
        { request: { path } },
      ).then((result) => result.canonicalPath),
    openManagedServerLoginItems: () =>
      invokeDesktop<void>("desktop_managed_server_open_login_items"),
    commitManagedServerAutoStart: (_nodeName) =>
      invokeDesktop<ManagedServerStatus>(
        "desktop_managed_server_set_auto_start",
        {
          enabled: true,
        },
      ),
    stopManagedServer: (disableAutoStart) =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_stop", {
        disableAutoStart,
      }),
    setManagedServerAutoStart: (enabled) =>
      invokeDesktop<ManagedServerStatus>(
        "desktop_managed_server_set_auto_start",
        {
          enabled,
        },
      ),
  };
}
