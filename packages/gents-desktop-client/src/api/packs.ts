import type {
  PackEditedChoice,
  PackInstallRequest,
  DynamicWorkflow,
  DynamicWorkflows,
  PackWireRequest,
  WorkflowPromoteRequest,
  WorkflowPromoted,
  PackWireResult,
  PackWireSources,
} from "../types/packs.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, PackCommand } from "./types.js";

export function createPackCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, PackCommand> {
  return {
    listInstalledPacks: () => invokeDesktop("desktop_pack_installed"),
    listPackPluginSlots: () => invokeDesktop("desktop_pack_plugin_slots"),
    readPackAccount: () => invokeDesktop("desktop_pack_whoami"),
    searchPacks: (query: string, page: number) =>
      invokeDesktop("desktop_pack_search", { query, page }),
    installPack: (request: PackInstallRequest) =>
      invokeDesktop("desktop_pack_install", { request }).then(() => undefined),
    updatePack: (pack: string, edited: PackEditedChoice) =>
      invokeDesktop("desktop_pack_update", { package: pack, edited }).then(
        () => undefined,
      ),
    removePack: (pack: string) =>
      invokeDesktop("desktop_pack_remove", { package: pack }).then(
        () => undefined,
      ),
    bindPackPlugin: (plugin: string, profile: string | null) =>
      invokeDesktop("desktop_pack_plugin_bind", {
        request: { plugin, profile },
      }).then(() => undefined),
    wirePack: (request: PackWireRequest) =>
      invokeDesktop("desktop_pack_wire", { request }).then(
        (report) => report as PackWireResult,
      ),
    listPackWireSources: () =>
      invokeDesktop("desktop_pack_wire_sources").then(
        (report) => report as PackWireSources,
      ),
    listDynamicWorkflows: () =>
      invokeDesktop("desktop_workflows_dynamic").then(
        (report) => report as DynamicWorkflows,
      ),
    getDynamicWorkflow: (selector: string) =>
      invokeDesktop("desktop_workflow_dynamic", { selector }).then(
        (report) => report as DynamicWorkflow,
      ),
    promoteWorkflow: (request: WorkflowPromoteRequest) =>
      invokeDesktop("desktop_workflow_promote", { request }).then(
        (report) => report as WorkflowPromoted,
      ),
    signInToPackRegistry: (token: string) =>
      invokeDesktop("desktop_pack_login", { token }).then(() => undefined),
    signOutOfPackRegistry: () =>
      invokeDesktop("desktop_pack_logout").then(() => undefined),
  };
}
