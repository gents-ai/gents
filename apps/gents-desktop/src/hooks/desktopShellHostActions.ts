import type {
  AllowedFolderAccess,
  DesktopApiAdapter,
  PackEditedChoice,
  PackInstallRequest,
  PluginApprovalDecision,
} from "@source-inc/gents-desktop-client";

/**
 * Packs, the host folders the agent's tools may use, and plugin calls asking
 * for more. Only the panel that asks shows each result, so these are plain
 * commands with no store: the panel keeps what it read and says what failed.
 */
export function createDesktopShellHostActions({ api }: { api: DesktopApiAdapter }) {
  return {
    listInstalledPacks: () => api.listInstalledPacks(),
    listPackPluginSlots: () => api.listPackPluginSlots(),
    readPackAccount: () => api.readPackAccount(),
    searchPacks: (query: string, page: number) => api.searchPacks(query, page),
    installPack: (request: PackInstallRequest) => api.installPack(request),
    updatePack: (pack: string, edited: PackEditedChoice) =>
      api.updatePack(pack, edited),
    removePack: (pack: string) => api.removePack(pack),
    bindPackPlugin: (plugin: string, profile: string | null) =>
      api.bindPackPlugin(plugin, profile),
    signInToPackRegistry: (token: string) => api.signInToPackRegistry(token),
    signOutOfPackRegistry: () => api.signOutOfPackRegistry(),
    listAllowedFolders: () => api.listAllowedFolders(),
    addAllowedFolder: (path: string, access: AllowedFolderAccess) =>
      api.addAllowedFolder(path, access),
    removeAllowedFolder: (path: string) => api.removeAllowedFolder(path),
    listPendingPluginApprovals: () => api.listPendingPluginApprovals(),
    decidePluginApproval: (id: string, decision: PluginApprovalDecision) =>
      api.decidePluginApproval(id, decision),
  };
}
