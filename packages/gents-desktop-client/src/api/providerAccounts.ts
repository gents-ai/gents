import type { BackendUsageView } from "../generated/BackendUsageView.js";
import type { ProviderAccountView } from "../generated/ProviderAccountView.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter } from "./types.js";

export function createProviderAccountCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Partial<DesktopApiAdapter> {
  return {
    listProviderAccounts: (nodeDid) =>
      invokeDesktop<ProviderAccountView[]>("desktop_provider_accounts_list", {
        request: { nodeDid },
      }),
    disconnectProviderAccount: (nodeDid, credentialId) =>
      invokeDesktop<void>("desktop_provider_account_disconnect", {
        request: { nodeDid, credentialId },
      }),
    retrySaveProviderAccount: (nodeDid, provider) =>
      invokeDesktop<ProviderAccountView>(
        "desktop_provider_account_retry_save",
        {
          request: { nodeDid, provider },
        },
      ),
    renameProviderAccount: (nodeDid, credentialId, label) =>
      invokeDesktop<void>("desktop_provider_account_rename", {
        request: { nodeDid, credentialId, label },
      }),
    removeProviderAccount: (nodeDid, credentialId) =>
      invokeDesktop<void>("desktop_provider_account_remove", {
        request: { nodeDid, credentialId },
      }),
    readProviderUsage: (nodeDid, refresh, provider) =>
      invokeDesktop<BackendUsageView[]>("desktop_provider_usage_read", {
        request: { nodeDid, refresh, provider: provider ?? null },
      }),
  };
}
