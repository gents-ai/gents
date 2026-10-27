import type {
  AllowedFolderAccess,
  DesktopApiAdapter,
  PackEditedChoice,
  PackInstallRequest,
  PackWireRequest,
  PluginApprovalDecision,
  WorkflowPromoteRequest,
  PackPluginCallRequest,
  UxDataCallRequest,
  UxDataSurfacesRequest,
} from "@source-inc/gents-desktop-client";

import { openInBrowser } from "../lib/externalLinks";
import { inNativeShell } from "../lib/nativeShell";

export function createHostActions({ api }: { api: DesktopApiAdapter }) {
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
    wirePack: (request: PackWireRequest) => api.wirePack(request),
    listPackWireSources: () => api.listPackWireSources(),
    listDynamicWorkflows: () => api.listDynamicWorkflows(),
    getDynamicWorkflow: (selector: string) => api.getDynamicWorkflow(selector),
    promoteWorkflow: (request: WorkflowPromoteRequest) => api.promoteWorkflow(request),
    signInToPackRegistry: (token: string) => api.signInToPackRegistry(token),
    signOutOfPackRegistry: () => api.signOutOfPackRegistry(),
    listAllowedFolders: () => api.listAllowedFolders(),
    addAllowedFolder: (path: string, access: AllowedFolderAccess) =>
      api.addAllowedFolder(path, access),
    removeAllowedFolder: (path: string) => api.removeAllowedFolder(path),
    subscribePluginApprovals: (token: string) => api.subscribePluginApprovals(token),
    unsubscribePluginApprovals: (token: string) =>
      api.unsubscribePluginApprovals(token),
    listPendingPluginApprovals: () => api.listPendingPluginApprovals(),
    decidePluginApproval: (id: string, decision: PluginApprovalDecision) =>
      api.decidePluginApproval(id, decision),
    listUxPlugins: () => api.listUxPlugins(),
    readUxPluginSource: (id: string) => api.readUxPluginSource(id),
    callPackPlugin: (request: PackPluginCallRequest) => api.callPackPlugin(request),
    callUxData: (request: UxDataCallRequest) => api.callUxData(request),
    listUxDataSurfaces: (request: UxDataSurfacesRequest) =>
      api.listUxDataSurfaces(request),
    async openExternalUrl(url: string) {
      if (inNativeShell() && api.openExternalUrl) {
        try {
          await api.openExternalUrl(url);
          return;
        } catch (error) {
          console.warn("bridge could not open the link", error);
        }
      }
      await openInBrowser(url);
    },
  };
}
