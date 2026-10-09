import type {
  FoundPack,
  InstalledPack,
  PackEditedChoice,
  PackInstallRequest,
  PackPluginSlot,
  PackSlotProfile,
} from "../types/packs.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, PackCommand } from "./types.js";

/* the operations `gents pack` runs, through the bridge */
export function createPackCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, PackCommand> {
  return {
    listInstalledPacks: () =>
      invokeDesktop<{ packs: InstalledPack[] }>("desktop_pack_installed"),
    listPackPluginSlots: () =>
      invokeDesktop<{ plugins: PackPluginSlot[]; profiles: PackSlotProfile[] }>(
        "desktop_pack_plugin_slots",
      ),
    readPackAccount: () =>
      invokeDesktop<{ account: { username: string } }>("desktop_pack_whoami"),
    searchPacks: (query: string, page: number) =>
      invokeDesktop<{ packs: FoundPack[]; has_more: boolean }>(
        "desktop_pack_search",
        { query, page },
      ),
    installPack: (request: PackInstallRequest) =>
      invokeDesktop<void>("desktop_pack_install", { request }),
    updatePack: (pack: string, edited: PackEditedChoice) =>
      invokeDesktop<void>("desktop_pack_update", { package: pack, edited }),
    removePack: (pack: string) =>
      invokeDesktop<void>("desktop_pack_remove", { package: pack }),
    bindPackPlugin: (plugin: string, profile: string | null) =>
      invokeDesktop<void>("desktop_pack_plugin_bind", {
        request: { plugin, profile },
      }),
    signInToPackRegistry: (token: string) =>
      invokeDesktop<void>("desktop_pack_login", { token }),
    signOutOfPackRegistry: () => invokeDesktop<void>("desktop_pack_logout"),
  };
}
