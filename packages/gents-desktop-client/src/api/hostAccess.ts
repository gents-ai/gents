import type {
  AllowedFolderAccess,
  PluginApprovalDecision,
} from "../types/hostAccess.js";
import type { createDesktopInvoker } from "./invoke.js";

  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, HostAccessCommand> {
  return {
    listAllowedFolders: () => invokeDesktop("desktop_allowed_dirs_list"),
    addAllowedFolder: (path: string, access: AllowedFolderAccess) =>
      invokeDesktop("desktop_allowed_dirs_add", { path, access }),
    removeAllowedFolder: (path: string) =>
      invokeDesktop("desktop_allowed_dirs_remove", { path }),
    subscribePluginApprovals: (token: string) =>
      invokeDesktop("desktop_plugin_approval_subscribe", { token }),
    unsubscribePluginApprovals: (token: string) =>
      invokeDesktop("desktop_plugin_approval_unsubscribe", { token }),
    listPendingPluginApprovals: () =>
      invokeDesktop("desktop_plugin_approvals_pending"),
    decidePluginApproval: (id: string, decision: PluginApprovalDecision) =>
      invokeDesktop("desktop_plugin_approval_decide", { id, decision }),
  };
}
