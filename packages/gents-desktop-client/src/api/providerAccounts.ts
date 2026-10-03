import type { BackendUsageView } from "../generated/BackendUsageView.js";
import type { ProviderAccountView } from "../generated/ProviderAccountView.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter } from "./types.js";

export function createProviderAccountCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Partial<DesktopApiAdapter> {
  return {
    listProviderAccounts: (agentDid) =>
      invokeDesktop<ProviderAccountView[]>("desktop_provider_accounts_list", {
        request: { agentDid },
      }),
    disconnectProviderAccount: (agentDid, credentialId) =>
      invokeDesktop<void>("desktop_provider_account_disconnect", {
        request: { agentDid, credentialId },
      }),
    retrySaveProviderAccount: (agentDid, provider) =>
      invokeDesktop<ProviderAccountView>(
        "desktop_provider_account_retry_save",
        {
          request: { agentDid, provider },
        },
      ),
    renameProviderAccount: (agentDid, credentialId, label) =>
      invokeDesktop<void>("desktop_provider_account_rename", {
        request: { agentDid, credentialId, label },
      }),
    removeProviderAccount: (agentDid, credentialId) =>
      invokeDesktop<void>("desktop_provider_account_remove", {
        request: { agentDid, credentialId },
      }),
    readProviderUsage: (agentDid, refresh, provider) =>
      invokeDesktop<BackendUsageView[]>("desktop_provider_usage_read", {
        request: { agentDid, refresh, provider: provider ?? null },
      }),
  };
}
