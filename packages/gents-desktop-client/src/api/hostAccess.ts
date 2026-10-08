import type {
  AllowedFolderAccess,
  AllowedFolders,
  PluginApprovalDecision,
  PluginApprovalRequest,
} from "../types/hostAccess.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, HostAccessCommand } from "./types.js";

/* the host folders the agent's tools may use, and plugin calls asking for
   more; each folder command answers with the list as it now stands */
export function createHostAccessCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, HostAccessCommand> {
  return {
    listAllowedFolders: () =>
      invokeDesktop<AllowedFolders>("desktop_allowed_dirs_list"),
    addAllowedFolder: (path: string, access: AllowedFolderAccess) =>
      invokeDesktop<AllowedFolders>("desktop_allowed_dirs_add", { path, access }),
    removeAllowedFolder: (path: string) =>
      invokeDesktop<AllowedFolders>("desktop_allowed_dirs_remove", { path }),
    listPendingPluginApprovals: () =>
      invokeDesktop<{ requests: PluginApprovalRequest[] }>(
        "desktop_plugin_approvals_pending",
      ),
    decidePluginApproval: (id: string, decision: PluginApprovalDecision) =>
      invokeDesktop<void>("desktop_plugin_approval_decide", { id, decision }),
  };
}
