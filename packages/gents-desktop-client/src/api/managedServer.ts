import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter } from "./types.js";

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
    managedServerStatus: () => invokeDesktop("desktop_managed_server_status"),
    startManagedServer: (nodeName, authority) =>
      invokeDesktop("desktop_managed_server_start", {
        request: {
          nodeName,
          toolCeiling: authority?.toolCeiling ?? null,
          toolRoot: authority?.toolRoot ?? null,
        },
      }),
    restartManagedServer: (nodeName, authority) =>
      invokeDesktop("desktop_managed_server_restart", {
        request: { nodeName, ...authority },
      }),
    resetManagedServer: (confirmation, disposition, fullReset) =>
      invokeDesktop("desktop_managed_server_reset", {
        request: {
          confirmation: confirmation ?? null,
          disposition: disposition ?? null,
          fullReset: fullReset ?? false,
        },
      }),
    validateManagedServerRoot: (path) =>
      invokeDesktop("desktop_managed_server_validate_root", {
        request: { path },
      }).then((result) => result.canonicalPath),
    openManagedServerLoginItems: () =>
      invokeDesktop("desktop_managed_server_open_login_items"),
    commitManagedServerAutoStart: (_nodeName) =>
      invokeDesktop("desktop_managed_server_set_auto_start", {
        enabled: true,
      }),
    stopManagedServer: (disableAutoStart) =>
      invokeDesktop("desktop_managed_server_stop", {
        disableAutoStart,
      }),
    setManagedServerAutoStart: (enabled) =>
      invokeDesktop("desktop_managed_server_set_auto_start", {
        enabled,
      }),
  };
}
