import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "./enabled";
import { dependentsWarning } from "./dependents";
import { useState } from "react";
import { SetupScreen } from "../setup/SetupScreen";
import { isProviderId, PROVIDER_VISUALS } from "../setup/InferenceSetup";
import type { ProviderId } from "../setup/inferenceSetupForm";
import { toast } from "sonner";
import type { InferenceBackendView } from "@source-inc/gents-desktop-client";
import {
  projectAgentOperationalStatus,
  selectedNodeReadinessDecision,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { ListDetail, type ListRow } from "./ListDetail";
import { RowMenu } from "./RowMenu";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Plus } from "lucide-react";
import { ProviderLogo } from "../ProviderLogo";
import { useApp } from "@/app/AppContext";
import { useAccounts, useProviderUsage, useSetupCatalog } from "@/hooks/useProviders";
import { healthy, KINDS, SUBSCRIPTION } from "./inferenceKinds";
import { UsageBar, AllProviderLimits } from "./ProviderUsage";
import { AccountDialogs, backendAccount, type AccountAction } from "./ProviderAccounts";
import { BackendEditor } from "./BackendEditor";
import { addItem, addingFrom } from "./draftItems";
import { leave, navigate } from "@/lib/router";

function providerOf(b: InferenceBackendView): ProviderId {
  switch (b.providerKind) {
    case "ChatGptCodex":
      return "openai";
    case "ClaudeCliSubscription":
    case "AnthropicApiKey":
      return "anthropic";
    case "XaiGrokOAuth":
      return "grok";
    case "OpenRouter":
      return "openrouter";
  }
  const host = b.endpoint ?? "";
  if (host.replace(/\/+$/, "").toLowerCase() === "https://api.x.ai/v1") return "grok";
  if (/openai\.com/.test(host)) return "openai";
  if (/anthropic\.com/.test(host)) return "anthropic";
  if (/openrouter/.test(host)) return "openrouter";
  return "local";
}

export function InferencePanel({
  deployment,
  item,
  under,
  orphans = [],
}: {
  deployment: NodeView;
  item?: string;
  under?: (b: InferenceBackendView) => ListRow[];
  orphans?: ListRow[];
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
    section: "inference",
  };
  const models = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
    section: "profiles",
  };
  const { accounts, reload } = useAccounts(deployment.nodeDid);
  const [acting, setActing] = useState<AccountAction | null>(null);
  const providerUsage = useProviderUsage(deployment.nodeDid, item);
  const usageOf = (backendId: string) =>
    providerUsage.usage.find((u) => u.backendIds.includes(backendId));
  const catalog = useSetupCatalog();
  const providers = catalog.providers;
  const readiness = selectedNodeReadinessDecision(deployment, null);
  const accountRecovery =
    readiness.kind === "unavailable" &&
    (readiness.reason === "credentials_required" ||
      readiness.reason === "subscription_not_entitled")
      ? projectAgentOperationalStatus(readiness, false)
      : null;
  const adding = addingFrom(item);
  const setAdding = (provider: ProviderId) =>
    navigate({ ...base, item: addItem(provider) });
  if (adding && isProviderId(adding)) {
    const here = { ...base, item: addItem(adding) };
    return (
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider={adding}
        nodeDid={deployment.nodeDid}
        onCancel={() => leave(here, models)}
        onDone={() => {
          leave(here, models);
          toast("Backend connected. Add a profile under it to use it.");
          void reload();
        }}
      />
    );
  }
  const order = (id: ProviderId) => providers.findIndex((p) => p.id === id);
  const configured = deployment.inferenceBackends
    .map((b) => ({ b, provider: providerOf(b), stored: backendAccount(accounts, b) }))
    .sort((x, y) => order(x.provider) - order(y.provider));
  const missing = providers.filter((p) => !configured.some((c) => c.provider === p.id));
  const rowMeta = (b: InferenceBackendView) => {
    const sub = SUBSCRIPTION[b.providerKind ?? ""];
    const stored = backendAccount(accounts, b);
    const label =
      stored?.label && stored.label !== (b.name ?? b.backendId)
        ? `${stored.label} · `
        : "";
    const cred = sub
      ? stored
        ? `${label}${b.enabled === false ? "off" : stored.enabled ? "signed in" : "disabled"}`
        : b.accountRef
          ? "account not on this node"
          : "not signed in"
      : b.apiKeyConfigured
        ? "key stored"
        : b.apiKeyEnvVar
          ? `key from ${b.apiKeyEnvVar}`
          : "no key";
    const profiles = deployment.inferenceProfiles.filter(
      (profile) => profile.backend_id === b.backendId,
    );
    return `${profiles.length} ${profiles.length === 1 ? "profile" : "profiles"} · ${KINDS.find((k) => k.value === b.providerKind)?.label ?? b.providerKind} · ${cred}`;
  };
  return (
    <>
      {accountRecovery && (
        <div
          role="status"
          data-testid="inference-account-recovery"
          className="mb-3 rounded-xl border border-border/60 bg-raised px-4 py-3"
        >
          <p className="text-sm font-medium">{accountRecovery.label}</p>
          <p className="text-xs text-muted-foreground">
            {accountRecovery.reason === "credentials_required"
              ? "Open the selected agent’s provider below and reconnect, or choose another configured account."
              : accountRecovery.detail}
          </p>
        </div>
      )}
      {catalog.error && !item && (
        <div
          role="alert"
          className="mb-4 flex items-center justify-between gap-3 rounded-2xl border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm"
        >
          <span>Couldn’t load the provider catalog: {catalog.error}</span>
          <Button variant="outline" size="sm" onClick={catalog.retry}>
            Retry
          </Button>
        </div>
      )}
      {!item && (
        <AllProviderLimits
          views={providerUsage.usage}
          onRefresh={() => providerUsage.refresh(null)}
        />
      )}
      <ListDetail
        base={base}
        item={item}
        back={{ route: models, label: "Providers" }}
        rows={[
          ...configured.map(({ b, provider, stored }) => ({
            id: b.backendId,
            children: under?.(b),
            metaLeadToggles: true,
            title: b.name ?? b.backendId,
            titleNote: (() => {
              const title =
                providers.find((p) => p.id === provider)?.displayName ??
                SUBSCRIPTION[b.providerKind ?? ""]?.title ??
                "";
              const name = (b.name ?? b.backendId).toLowerCase();
              return name.includes(title.toLowerCase()) ||
                name.includes(title.toLowerCase().replace(/\s+/g, ""))
                ? undefined
                : title;
            })(),
            meta: rowMeta(b),
            icon: <ProviderLogo kind={b.providerKind} endpoint={b.endpoint} />,
            badge: healthy(b.probeStatus) ? undefined : (b.probeStatus ?? undefined),
            badgeTone: "bad" as const,
            trailing: (
              <>
                {(!SUBSCRIPTION[b.providerKind ?? ""] || stored?.enabled) && (
                  <UsageBar view={usageOf(b.backendId)} />
                )}
                <RowMenu
                  name={b.name ?? b.backendId}
                  base={base}
                  id={b.backendId}
                  enabled={{
                    checked: b.enabled !== false,
                    onChange: (enabled) =>
                      setEnabled(
                        changeConfig,
                        deployment.nodeDid,
                        "InferenceBackend",
                        b.backendId,
                        enabled,
                      ),
                  }}
                  onDelete={
                    b.accountRef && stored
                      ? undefined
                      : () =>
                          changeConfig("deleteBackendConfig", {
                            backendId: b.backendId,
                            nodeDid: deployment.nodeDid,
                          })
                  }
                  warning={dependentsWarning(deployment, "backend", b.backendId)}
                >
                  {providers.some((option) => option.id === provider) && (
                    <DropdownMenuItem onClick={() => setAdding(provider)}>
                      Add another{" "}
                      {providers.find((x) => x.id === provider)?.displayName}
                    </DropdownMenuItem>
                  )}
                  {stored && (
                    <>
                      <DropdownMenuItem
                        onClick={() => setActing({ action: "rename", account: stored })}
                      >
                        Rename account…
                      </DropdownMenuItem>
                      {stored.enabled && (
                        <DropdownMenuItem
                          onClick={() =>
                            setActing({ action: "disconnect", account: stored })
                          }
                        >
                          Disconnect…
                        </DropdownMenuItem>
                      )}
                      <DropdownMenuItem
                        variant="destructive"
                        onClick={() => setActing({ action: "remove", account: stored })}
                      >
                        Remove account…
                      </DropdownMenuItem>
                    </>
                  )}
                </RowMenu>
              </>
            ),
          })),
          ...orphans,
          ...missing.map((p) => ({
            id: `setup:${p.id}`,
            title: p.displayName,
            meta: p.description,
            icon: (
              <img
                src={PROVIDER_VISUALS[p.id].logo}
                alt=""
                className="size-4 opacity-70 dark:invert"
              />
            ),
            badge: accounts.some(
              (account) =>
                account.enabled &&
                !account.pendingSave &&
                Object.values(SUBSCRIPTION).some(
                  (subscription) =>
                    subscription.providerId === p.id &&
                    subscription.provider === account.provider,
                ),
            )
              ? "Signed in · finish setup"
              : "Not set up",
            onOpen: () => setAdding(p.id),
          })),
        ]}
        createLabel=""
        empty="No providers."
        createMenu={
          <DropdownMenu>
            <DropdownMenuTrigger render={<Button variant="outline" />}>
              <Plus /> New backend
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-auto min-w-52">
              <DropdownMenuGroup>
                {providers.map((p) => (
                  <DropdownMenuItem key={p.id} onClick={() => setAdding(p.id)}>
                    <img
                      src={PROVIDER_VISUALS[p.id].logo}
                      alt=""
                      className="size-4 opacity-70 dark:invert"
                    />
                    {p.displayName}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        }
        detail={(id) => {
          const backend = deployment.inferenceBackends.find((b) => b.backendId === id);
          if (!backend) return null;
          return (
            <BackendEditor
              key={backend.backendId}
              deployment={deployment}
              backend={backend}
              accounts={accounts}
              usage={{
                view: usageOf(backend.backendId),
                refresh: providerUsage.refresh,
              }}
            />
          );
        }}
      />
      <AccountDialogs
        deployment={deployment}
        accounts={accounts}
        acting={acting}
        onClose={() => setActing(null)}
      />
    </>
  );
}
