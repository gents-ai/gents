/* Inference: backends and the provider accounts that some of them need,
   in one list. A row is a backend; its kind decides the credential
   section. OpenAI-compatible and OpenRouter take a key or an env var;
   ChatGPT/Codex and Grok exist only through a subscription sign-in, so
   the account card sits in the row with connect, cancel and disconnect.
   Everything else is the desktop app's Backends panel field for field. */
import { useEffect, useState } from "react";
import { SetupScreen } from "../setup/SetupScreen";
import { toast } from "sonner";
import type {
  BackendProviderKind,
  BackendSaveRequest,
  DeploymentView,
  InferenceBackend,
  InferenceBackendView,
  OpenAiWireApi,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import type { Shell } from "@/hooks/useShell";
import { PROVIDER_CREDENTIAL_KIND } from "@/lib/providerLogin";
import {
  fromLinesOrNull,
  optionalInteger,
  requiredHttpUrl,
  str,
  toLines,
  useDraft,
} from "./draft";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  SwitchRow,
  TextRow,
} from "./editors";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group, Row } from "./rows";
import { ProviderLogo } from "../ProviderLogo";

export function backendSave(
  agentDid: string,
  fields: {
    backendId: string;
    name: string;
    providerKind: string;
    openaiWireApi: string | null;
    endpoint: string;
    apiKey: string | null;
    apiKeyEnvVar: string | null;
    connectTimeoutSecs: number | null;
    discoveryTimeoutSecs: number | null;
    maxConcurrent: number | null;
    maxQueueDepth: number | null;
    enabled: boolean | null;
  },
): BackendSaveRequest {
  return {
    document: {
      agent_did: agentDid,
      backend_id: fields.backendId,
      name: fields.name,
      provider_kind: fields.providerKind as BackendProviderKind,
      openai_wire_api: (fields.openaiWireApi as OpenAiWireApi | null) ?? null,
      endpoint: fields.endpoint,
      auth: isSubscriptionKind(fields.providerKind)
        ? { kind: "principal_oauth" }
        : fields.apiKey
          ? { kind: "api_key", key: fields.apiKey }
          : fields.apiKeyEnvVar
            ? { kind: "environment", variable: fields.apiKeyEnvVar }
            : { kind: "unauthenticated" },
      connect_timeout_secs: fields.connectTimeoutSecs,
      discovery_timeout_secs: fields.discoveryTimeoutSecs,
      max_concurrent: fields.maxConcurrent,
      max_queue_depth: fields.maxQueueDepth,
      enabled: fields.enabled,
    },
  };
}

const isSubscriptionKind = (kind: string) =>
  kind === "ChatGptCodex" ||
  kind === "XaiGrokOAuth" ||
  kind === "ClaudeCliSubscription";

const healthy = (status: string | null) => status === "healthy" || status === "ok";

const KINDS = [
  { value: "OpenAiCompatible", label: "OpenAI compatible" },
  { value: "OpenRouter", label: "OpenRouter" },
  { value: "ChatGptCodex", label: "ChatGPT / Codex (subscription)" },
  { value: "XaiGrokOAuth", label: "Grok (subscription)" },
  { value: "ClaudeCliSubscription", label: "Anthropic / Claude (subscription)" },
];
/* subscription kinds, and the provider name their account carries */
const SUBSCRIPTION: Record<
  string,
  { provider: string; title: string; note: string; login: "codex" | "grok" | "claude" }
> = {
  ChatGptCodex: {
    provider: PROVIDER_CREDENTIAL_KIND.openai,
    title: "ChatGPT / Codex",
    note: "Use an eligible ChatGPT subscription for Codex inference.",
    login: "codex",
  },
  XaiGrokOAuth: {
    provider: PROVIDER_CREDENTIAL_KIND.grok,
    title: "Grok / xAI",
    note: "Use SuperGrok or an eligible X Premium+ subscription.",
    login: "grok",
  },
  ClaudeCliSubscription: {
    provider: PROVIDER_CREDENTIAL_KIND.anthropic,
    title: "Anthropic / Claude",
    note: "Use a Claude Pro or Max subscription.",
    login: "claude",
  },
};

function useAccounts(shell: Shell, agentDid: string) {
  const [accounts, setAccounts] = useState<ProviderAccountView[]>([]);
  const api = shell.api;
  const load = () =>
    (api.listProviderAccounts?.(agentDid) ?? Promise.resolve([])).then(
      setAccounts,
      () => setAccounts([]),
    );
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentDid, shell.snapshot]);
  return { accounts, reload: load };
}

/* the account card for a subscription backend */
function AccountRows({
  shell,
  deployment,
  kind,
  accounts,
  reload,
}: {
  shell: Shell;
  deployment: DeploymentView;
  kind: string;
  accounts: ProviderAccountView[];
  reload: () => Promise<void>;
}) {
  const sub = SUBSCRIPTION[kind]!;
  const account = accounts.find((a) => a.provider === sub.provider && a.enabled);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  const expired = account ? Date.parse(account.accessTokenExpiresAt) < now : false;
  const [busy, setBusy] = useState(false);
  const [confirmingDisconnect, setConfirmingDisconnect] = useState(false);
  const api = shell.api;
  const signIn = async () => {
    setBusy(true);
    try {
      if (sub.login === "codex") await api.codexLogin(deployment.agentDid);
      else if (sub.login === "claude") await api.claudeLogin(deployment.agentDid);
      else await api.grokLogin(deployment.agentDid);
      toast("Signed in");
      await reload();
    } catch (error) {
      toast(
        `Sign in failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setBusy(false);
    }
  };
  const disconnect = async () => {
    if (!account) return;
    setBusy(true);
    try {
      await api.disconnectProviderAccount?.(deployment.agentDid, account.credentialId);
      toast("Disconnected");
      setConfirmingDisconnect(false);
      await reload();
    } catch (error) {
      toast(
        `Disconnect failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <Row label="Account" description={sub.note}>
        <span className="flex items-center gap-2">
          {account && (
            <Badge variant={expired ? "destructive" : "secondary"}>
              {expired ? "Expired" : "Connected"}
            </Badge>
          )}
          {account && !confirmingDisconnect && (
            <Button
              size="sm"
              variant="quiet"
              disabled={busy}
              onClick={() => setConfirmingDisconnect(true)}
            >
              Disconnect
            </Button>
          )}
          {account && confirmingDisconnect && (
            <>
              <span className="text-xs text-muted-foreground">
                Disconnect {sub.title}?
              </span>
              <Button
                size="sm"
                variant="quiet"
                disabled={busy}
                onClick={() => setConfirmingDisconnect(false)}
              >
                Keep connected
              </Button>
              <Button
                size="sm"
                variant="destructive"
                disabled={busy}
                onClick={disconnect}
              >
                {busy ? "Disconnecting…" : "Disconnect now"}
              </Button>
            </>
          )}
          <Button
            size="sm"
            variant={account ? "outline" : "brand"}
            disabled={busy}
            onClick={signIn}
          >
            {busy ? "Signing in…" : account ? "Reconnect" : "Connect"}
          </Button>
        </span>
      </Row>
      {account && (
        <>
          <FactRow label="Signed in as">
            {account.accountId ?? "Account identity unavailable — reconnect to refresh"}
            {account.planType ? ` · ${account.planType}` : ""}
          </FactRow>
          <FactRow label="Expires">
            {new Date(account.accessTokenExpiresAt).toLocaleString()} ·{" "}
            {expired
              ? "expired"
              : `in ${Math.max(1, Math.ceil((Date.parse(account.accessTokenExpiresAt) - Date.now()) / 60000))} minutes`}
          </FactRow>
        </>
      )}
    </>
  );
}

function Editor({
  shell,
  deployment,
  backend,
  accounts,
  reload,
}: {
  shell: Shell;
  deployment: DeploymentView;
  backend: InferenceBackendView;
  accounts: ProviderAccountView[];
  reload: () => Promise<void>;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "inference",
  };
  const saved = {
    name: backend.name ?? "",
    providerKind: backend.providerKind ?? "OpenAiCompatible",
    openaiWireApi: backend.openaiWireApi ?? "",
    endpoint: backend.endpoint ?? "",
    apiKeyEnvVar: backend.apiKeyEnvVar ?? "",
    apiKey: "",
    connectTimeoutSecs: str(backend.connectTimeoutSecs ?? 10),
    discoveryTimeoutSecs: str(backend.discoveryTimeoutSecs ?? 10),
    maxConcurrent: str(backend.maxConcurrent),
    maxQueueDepth: str(backend.maxQueueDepth),
    enabled: backend.enabled ?? true,
    tags: toLines(backend.tags),
  };
  const d = useDraft(saved, async (next) => {
    const name = next.name.trim();
    if (!name) throw new Error("Name is required");
    const endpoint =
      next.providerKind === "ClaudeCliSubscription" &&
      next.endpoint === "claude-cli://subscription"
        ? next.endpoint
        : requiredHttpUrl("Endpoint", next.endpoint);
    if (next.apiKey.trim() && next.apiKeyEnvVar.trim())
      throw new Error("Choose an API key or an environment variable, not both");
    const maxConcurrent = optionalInteger("Max concurrent", next.maxConcurrent, {
      min: 1,
    });
    const maxQueueDepth = optionalInteger("Max queue depth", next.maxQueueDepth, {
      min: 0,
    });
    const connectTimeoutSecs = optionalInteger(
      "Connect timeout",
      next.connectTimeoutSecs,
      { min: 1 },
    );
    const discoveryTimeoutSecs = optionalInteger(
      "Discovery timeout",
      next.discoveryTimeoutSecs,
      { min: 1 },
    );
    let auth: InferenceBackend["auth"] | undefined;
    if (isSubscriptionKind(next.providerKind)) auth = { kind: "principal_oauth" };
    else if (next.apiKey.trim()) auth = { kind: "api_key", key: next.apiKey };
    else if (next.apiKeyEnvVar.trim())
      auth = { kind: "environment", variable: next.apiKeyEnvVar.trim() };
    else if (!backend.apiKeyConfigured || isSubscriptionKind(saved.providerKind))
      auth = { kind: "unauthenticated" };

    const changes: Partial<Omit<InferenceBackend, "agent_did" | "backend_id">> = {
      name,
      provider_kind: next.providerKind as BackendProviderKind,
      openai_wire_api: (next.openaiWireApi as OpenAiWireApi) || null,
      endpoint,
      connect_timeout_secs: connectTimeoutSecs,
      discovery_timeout_secs: discoveryTimeoutSecs,
      max_concurrent: maxConcurrent,
      max_queue_depth: maxQueueDepth,
      enabled: next.enabled,
      tags: fromLinesOrNull(next.tags),
    };
    if (auth) changes.auth = auth;
    await shell.applyConfig((api) =>
      api.patchConfigComponents({
        agentDid: deployment.agentDid,
        patches: [{ collection: "InferenceBackend", id: backend.backendId, changes }],
      }),
    );
  });
  const [probe, setProbe] = useState<string | null>(null);
  const [discoveredModels, setDiscoveredModels] = useState<string[] | null>(null);
  const id = (f: string) => `${backend.backendId}-${f}`;
  const subscription = d.draft.providerKind in SUBSCRIPTION;
  const users = deployment.inferenceProfiles
    .filter((p) => p.backend_id === backend.backendId)
    .map((p) => p.display_name ?? p.profile_id);
  return (
    <>
      <Group
        title={backend.name ?? backend.backendId}
        action={
          <span className="flex items-center gap-2">
            <ProviderLogo
              kind={d.draft.providerKind}
              endpoint={d.draft.endpoint}
              className="mr-1"
            />
            {subscription && <Badge variant="secondary">Subscription</Badge>}
            <Badge variant={healthy(backend.probeStatus) ? "secondary" : "destructive"}>
              {backend.probeStatus ?? "unprobed"}
            </Badge>
            <Button
              size="sm"
              variant="outline"
              onClick={async () => {
                try {
                  if (subscription) {
                    setProbe("Discovering models…");
                    const provider =
                      d.draft.providerKind === "ChatGptCodex"
                        ? "openai"
                        : d.draft.providerKind === "ClaudeCliSubscription"
                          ? "anthropic"
                          : "grok";
                    const authMethod =
                      provider === "openai"
                        ? "chat_gpt_oauth"
                        : provider === "anthropic"
                          ? "claude_oauth"
                          : "grok_oauth";
                    const result = await shell.api.discoverInferenceModels({
                      requestKey: `backend-${backend.backendId}-${Date.now()}`,
                      agentDid: deployment.agentDid,
                      provider,
                      authMethod,
                      endpoint: d.draft.endpoint,
                      apiKey: null,
                    });
                    if (!result.reachable)
                      throw new Error(result.failure?.message ?? "Discovery failed");
                    setDiscoveredModels(
                      result.models.map((option) => option.advertised.model_name),
                    );
                    setProbe(`Authenticated · ${result.models.length} models`);
                    return;
                  }
                  const endpoint = requiredHttpUrl("Endpoint", d.draft.endpoint);
                  setProbe("probing…");
                  const r = await shell.api.probeInferenceEndpoint(endpoint);
                  setProbe(
                    r.reachable
                      ? `reachable · ${r.models.length} models`
                      : "unreachable",
                  );
                  toast(r.reachable ? "Endpoint reachable" : "Endpoint unreachable");
                } catch (error) {
                  setProbe("probe failed");
                  toast(
                    `Probe failed: ${error instanceof Error ? error.message : String(error)}`,
                  );
                }
              }}
            >
              {subscription ? "Refresh models" : "Probe"}
            </Button>
          </span>
        }
      >
        <FactRow label="Backend ID" mono>
          {backend.backendId}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Name"
          value={d.draft.name}
          onChange={(v) => d.set("name", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("kind")}
          label="Provider kind"
          description="Decides how it is paid for: a key, or a subscription sign-in."
          value={d.draft.providerKind}
          onChange={(v) => d.choose("providerKind", v)}
          items={KINDS}
        />
        {!subscription && (
          <ChoiceRow
            id={id("wire")}
            label="OpenAI wire API"
            description="Leave automatic unless the endpoint requires one protocol."
            value={d.draft.openaiWireApi}
            onChange={(v) => d.choose("openaiWireApi", v)}
            items={[
              { value: "responses", label: "Responses" },
              { value: "chat_completions", label: "Chat completions" },
            ]}
            none="Automatic"
          />
        )}
        <FactRow
          label="Used by"
          description="Delete is blocked while a behaviour points here."
        >
          {users.length ? users.join(", ") : "no behaviour"}
        </FactRow>
      </Group>
      <Group title={subscription ? "Subscription" : "Credential"}>
        {subscription ? (
          <AccountRows
            shell={shell}
            deployment={deployment}
            kind={d.draft.providerKind}
            accounts={accounts}
            reload={reload}
          />
        ) : (
          <>
            <TextRow
              id={id("env")}
              label="API key env var"
              description="Read from the environment at start."
              value={d.draft.apiKeyEnvVar}
              onChange={(v) => d.set("apiKeyEnvVar", v)}
              onCommit={d.commit}
              onEnter={d.onEnter}
              placeholder="OPENAI_API_KEY"
              mono
            />
            <TextRow
              id={id("key")}
              label="API key"
              description={
                backend.apiKeyConfigured
                  ? "A key is stored; enter one to replace it."
                  : "Stored by the bridge, never shown again."
              }
              value={d.draft.apiKey}
              onChange={(v) => d.set("apiKey", v)}
              onCommit={d.commit}
              onEnter={d.onEnter}
              placeholder={backend.apiKeyConfigured ? "Configured" : "sk-…"}
              password
            />
            {backend.apiKeyConfigured && (
              <Row label="Stored key">
                <Button
                  size="sm"
                  variant="quiet"
                  onClick={() =>
                    shell
                      .applyConfig((api) =>
                        api.patchConfigComponents({
                          agentDid: deployment.agentDid,
                          patches: [
                            {
                              collection: "InferenceBackend",
                              id: backend.backendId,
                              changes: { auth: { kind: "unauthenticated" } },
                            },
                          ],
                        }),
                      )
                      .then(() => toast("Stored key cleared"))
                      .catch((error) =>
                        toast(
                          `Clear failed: ${error instanceof Error ? error.message : String(error)}`,
                        ),
                      )
                  }
                >
                  Clear stored key
                </Button>
              </Row>
            )}
          </>
        )}
      </Group>
      <Group title="Endpoint and models">
        <TextRow
          id={id("endpoint")}
          label="Endpoint"
          value={d.draft.endpoint}
          onChange={(v) => d.set("endpoint", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          placeholder="https://…/v1"
          mono
          wide
        />
        {probe && <FactRow label="Last probe">{probe}</FactRow>}
        <FactRow
          label="Advertised models"
          description="Runtime discovery owns this catalog. Select a model on a profile."
          mono
        >
          {(discoveredModels ?? backend.models).length ? (
            <span className="flex flex-col gap-1 whitespace-normal">
              {(discoveredModels ?? backend.models).map((model) => (
                <span key={model}>{model}</span>
              ))}
            </span>
          ) : (
            "No catalog yet — refresh models"
          )}
        </FactRow>
        <NumberRow
          id={id("connect-timeout")}
          label="Connect timeout seconds"
          description="Positive whole number, or blank for the runtime default."
          value={d.draft.connectTimeoutSecs}
          onChange={(v) => d.set("connectTimeoutSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("discovery-timeout")}
          label="Discovery timeout seconds"
          description="Positive whole number, or blank for the runtime default."
          value={d.draft.discoveryTimeoutSecs}
          onChange={(v) => d.set("discoveryTimeoutSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("conc")}
          label="Max concurrent"
          description="Whole number of 1 or more."
          value={d.draft.maxConcurrent}
          onChange={(v) => d.set("maxConcurrent", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("queue")}
          label="Max queue depth"
          description="Whole number of 0 or more; 0 disables queueing."
          value={d.draft.maxQueueDepth}
          onChange={(v) => d.set("maxQueueDepth", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <AreaRow
          id={id("tags")}
          label="Tags"
          description="One optional discovery label per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={2}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      <DeleteButton
        label={backend.name ?? backend.backendId}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteBackendConfig({
              backendId: backend.backendId,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function InferencePanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell;
  deployment: DeploymentView;
  item?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "inference",
  };
  const { accounts, reload } = useAccounts(shell, deployment.agentDid);
  const [adding, setAdding] = useState(false);
  if (adding)
    return (
      <SetupScreen
        shell={shell}
        initialStep="inference"
        purpose="add-backend"
        agentDid={deployment.agentDid}
        onCancel={() => setAdding(false)}
        onDone={() => {
          setAdding(false);
          void reload();
        }}
      />
    );
  const rowMeta = (b: InferenceBackendView) => {
    const sub = SUBSCRIPTION[b.providerKind ?? ""];
    const account = sub && accounts.find((a) => a.provider === sub.provider);
    const cred = sub
      ? account
        ? "signed in"
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
    <ListDetail
      base={base}
      item={item}
      rows={deployment.inferenceBackends.map((b) => ({
        id: b.backendId,
        title: b.name ?? b.backendId,
        meta: rowMeta(b),
        icon: <ProviderLogo kind={b.providerKind} endpoint={b.endpoint} />,
        badge: b.probeStatus ?? undefined,
        badgeTone: healthy(b.probeStatus) ? "default" : "bad",
      }))}
      createLabel="New backend"
      empty="No inference yet. Add a backend: a local server, a key, or a subscription."
      onCreate={() => setAdding(true)}
      detail={(id) => {
        const backend = deployment.inferenceBackends.find((b) => b.backendId === id)!;
        return (
          <Editor
            key={backend.backendId}
            shell={shell}
            deployment={deployment}
            backend={backend}
            accounts={accounts}
            reload={reload}
          />
        );
      }}
    />
  );
}
