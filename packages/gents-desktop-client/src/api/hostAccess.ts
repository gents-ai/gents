import type {
  AllowedFolderAccess,
  AllowedFolders,
  PluginApprovalDecision,
  PluginApprovalRequest,
} from "../types/hostAccess.js";
import type { createDesktopInvoker } from "./invoke.js";

  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, HostAccessCommand> {
  return {
    listAllowedFolders: () =>
      invokeDesktop<AllowedFolders>("desktop_allowed_dirs_list"),
    addAllowedFolder: (path: string, access: AllowedFolderAccess) =>
      invokeDesktop<AllowedFolders>("desktop_allowed_dirs_add", {
        path,
        access,
      }),
    removeAllowedFolder: (path: string) =>
      invokeDesktop<AllowedFolders>("desktop_allowed_dirs_remove", { path }),
    subscribePluginApprovals: (token: string) =>
      invokeDesktop<void>("desktop_plugin_approval_subscribe", { token }),
    unsubscribePluginApprovals: (token: string) =>
      invokeDesktop<void>("desktop_plugin_approval_unsubscribe", { token }),
    listPendingPluginApprovals: () =>
      invokeDesktop<{ requests: PluginApprovalRequest[] }>(
        "desktop_plugin_approvals_pending",
      ),
    decidePluginApproval: (id: string, decision: PluginApprovalDecision) =>
      invokeDesktop<void>("desktop_plugin_approval_decide", { id, decision }),
  };
}
