import type {
  PackPluginCallRequest,
  UxPluginListing,
  UxPluginSource,
} from "../types/uxPlugins.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, UxPluginCommand } from "./types.js";

/* the UX plugin doors: the inventory on disk, one plugin's module when it
   is enabled, and the pack .afb call a plugin's page makes under its own
   listing; the bridge takes the call's fields under `request` */
export function createUxPluginCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, UxPluginCommand> {
  return {
    listUxPlugins: () =>
      invokeDesktop<{ plugins: UxPluginListing[] }>("desktop_ux_plugins_list"),
    readUxPluginSource: (id: string) =>
      invokeDesktop<UxPluginSource>("desktop_ux_plugin_source", { id }),
    callPackPlugin: (request: PackPluginCallRequest) =>
      invokeDesktop<{ output: unknown }>("desktop_plugin_call", { request }),
  };
}
