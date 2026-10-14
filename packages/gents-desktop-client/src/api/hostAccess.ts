import type {
  AllowedFolderAccess,
  PluginApprovalDecision,
} from "../types/hostAccess.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, HostAccessCommand } from "./types.js";

/* the host folders the agent's tools may use, and plugin calls asking for
   more; each folder command answers with the list as it now stands */
export function createHostAccessCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, HostAccessCommand> {
  return {
    listAllowedFolders: () => invokeDesktop("desktop_allowed_dirs_list"),
    addAllowedFolder: (path: string, access: AllowedFolderAccess) =>
      invokeDesktop("desktop_allowed_dirs_add", { path, access }),
    removeAllowedFolder: (path: string) =>
      invokeDesktop("desktop_allowed_dirs_remove", { path }),
    listPendingPluginApprovals: () =>
      invokeDesktop("desktop_plugin_approvals_pending"),
    decidePluginApproval: (id: string, decision: PluginApprovalDecision) =>
      invokeDesktop("desktop_plugin_approval_decide", { id, decision }),
  };
}
