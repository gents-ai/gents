import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter } from "./types.js";

export function createProviderAccountCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Partial<DesktopApiAdapter> {
  return {
    fetchProviderRecovery: (request) =>
      invokeDesktop("desktop_provider_recovery_plan", { request }),
    applyProviderRecovery: (request) =>
      invokeDesktop("desktop_provider_recovery_apply", { request }),
    listProviderAccounts: (nodeDid) =>
      invokeDesktop("desktop_provider_accounts_list", {
        request: { nodeDid },
      }),
    disconnectProviderAccount: (nodeDid, credentialId) =>
      invokeDesktop("desktop_provider_account_disconnect", {
        request: { nodeDid, credentialId },
      }),
    retrySaveProviderAccount: (nodeDid, provider) =>
      invokeDesktop("desktop_provider_account_retry_save", {
        request: { nodeDid, provider },
      }),
    renameProviderAccount: (nodeDid, credentialId, label) =>
      invokeDesktop("desktop_provider_account_rename", {
        request: { nodeDid, credentialId, label },
      }),
    removeProviderAccount: (nodeDid, credentialId) =>
      invokeDesktop("desktop_provider_account_remove", {
        request: { nodeDid, credentialId },
      }),
    readProviderUsage: (nodeDid, refresh, provider) =>
      invokeDesktop("desktop_provider_usage_read", {
        request: { nodeDid, refresh, provider: provider ?? null },
      }),
  };
}
