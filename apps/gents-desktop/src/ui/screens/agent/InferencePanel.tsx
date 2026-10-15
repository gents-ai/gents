/* Inference: backends and the provider accounts that some of them need,
   in one list. A row is a backend; its kind decides the credential
   section. OpenAI-compatible and OpenRouter take a key or an env var;
   ChatGPT/Codex and Grok exist only through a subscription sign-in, so
   the account card sits in the row with connect, cancel and disconnect. */
import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "./enabled";
import { dependentsWarning } from "./dependents";
import { useState } from "react";
import { SetupScreen } from "../setup/SetupScreen";
import { PROVIDER_VISUALS } from "../setup/InferenceSetup";
import type { ProviderId } from "../setup/inferenceSetupForm";
import { toast } from "sonner";
import type { InferenceBackendView } from "@source-inc/gents-desktop-client";
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
import { UsageBar } from "./ProviderUsage";
import { AccountDialogs, backendAccount, type AccountAction } from "./ProviderAccounts";
import { BackendEditor } from "./BackendEditor";

/* which catalog provider a configured backend belongs to */
function providerOf(b: InferenceBackendView): ProviderId {
  switch (b.providerKind) {
    case "ChatGptCodex":
      return "openai";
    case "ClaudeCliSubscription":
      return "anthropic";
    case "XaiGrokOAuth":
      return "grok";
    case "OpenRouter":
      return "openrouter";
  }
  const host = b.endpoint ?? "";
  if (/openai\.com/.test(host)) return "openai";
  if (/anthropic\.com/.test(host)) return "anthropic";
  if (/openrouter/.test(host)) return "openrouter";
  return "local";
}

/* The backends: the provider catalog, every provider a row. A configured
   one shows its backend (state, switch, menu); one not set up opens the
   provider step with it chosen. The editor sits behind the `inference`
   route; Back from it goes to Models. */
export function InferencePanel({
  deployment,
  item,
  under,
  orphans = [],
}: {
  deployment: NodeView;
  item?: string;
  /* rows to nest under a configured backend: its models */
  under?: (b: InferenceBackendView) => ListRow[];
  /* profiles whose backend no longer exists, listed after the backends */
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
  const providerUsage = useProviderUsage(deployment.nodeDid);
  const usageOf = (backendId: string) =>
    providerUsage.usage.find((u) => u.backendId === backendId);
  const catalog = useSetupCatalog();
  const providers = catalog.providers;
  /* the provider whose inputs are open, from a catalog row or Add another */
  const [adding, setAdding] = useState<ProviderId | null>(null);
  if (adding)
    return (
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider={adding}
        nodeDid={deployment.nodeDid}
        onCancel={() => setAdding(null)}
        onDone={() => {
          /* back to the catalog: the new backend's row now offers Add profile */
          setAdding(null);
          toast("Backend connected. Add a profile under it to use it.");
          void reload();
        }}
      />
    );
  /* the catalog: configured backends under their provider, then the rest */
  /* backends in the catalog's provider order, so two of one provider sit together */
  const order = (id: ProviderId) => providers.findIndex((p) => p.id === id);
  const configured = deployment.inferenceBackends
    .map((b) => ({ b, provider: providerOf(b), stored: backendAccount(accounts, b) }))
    .sort((x, y) => order(x.provider) - order(y.provider));
  const missing = providers.filter((p) => !configured.some((c) => c.provider === p.id));
  const rowMeta = (b: InferenceBackendView) => {
    const sub = SUBSCRIPTION[b.providerKind ?? ""];
    const stored = backendAccount(accounts, b);
    /* the label only where the row's title does not already say it */
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
            /* the provider, only when the backend's name does not already say it */
            titleNote: (() => {
              const title = providers.find((p) => p.id === provider)?.displayName ?? "";
              const name = (b.name ?? b.backendId).toLowerCase();
              return name.includes(title.toLowerCase()) ||
                name.includes(title.toLowerCase().replace(/\s+/g, ""))
                ? undefined
                : title;
            })(),
            meta: rowMeta(b),
            icon: <ProviderLogo kind={b.providerKind} endpoint={b.endpoint} />,
            /* only trouble is worth a badge; a healthy backend just has its switch on */
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
                  /* an added account's backend goes with Remove account */
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
                  <DropdownMenuItem onClick={() => setAdding(provider)}>
                    Add another{" "}
                    {providers.find((x) => x.id === provider)?.displayName ?? "backend"}
                  </DropdownMenuItem>
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
            badge: "Not set up",
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
          /* the list also holds profiles and providers not set up yet;
             only a backend opens here */
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
