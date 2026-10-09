/* One backend's editor: its kind, endpoint and credential, the account a
   subscription runs on, and its models and usage. */
import type { NodeView } from "../../../hooks/fleetStore";
import { dependentsWarning } from "./dependents";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { newestWins } from "../../../lib/reads";
import type {
  BackendProviderKind,
  BackendSaveRequest,
  BackendUsageView,
  InferenceBackend,
  InferenceBackendView,
  OpenAiWireApi,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { setupErrorMessage } from "../../../lib/setupErrors";
import { optionalInteger, requiredHttpUrl, str, useDraft, problemOf } from "./draft";
import {
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  SwitchRow,
  TextRow,
  TagsRow,
} from "./editors";
import { DeleteButton } from "./ListDetail";
import { Group, Row } from "./rows";
import { ProviderLogo } from "../ProviderLogo";
import { useApp } from "@/app/AppContext";
import { toastFailure } from "@/lib/failure";
import { healthy, isSubscriptionKind, KINDS, SUBSCRIPTION } from "./inferenceKinds";
import { UsageRows } from "./ProviderUsage";
import { accountWarnings, AccountRows, backendAccount } from "./ProviderAccounts";

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

export function BackendEditor({
  deployment,
  backend,
  accounts,
  usage,
  embedded = false,
}: {
  deployment: NodeView;
  backend: InferenceBackendView;
  accounts: readonly ProviderAccountView[];
  /* this backend's usage and the read again; absent, no Usage group */
  usage?: {
    view?: BackendUsageView;
    refresh: (provider: string | null) => Promise<void>;
  };
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const {
    actions: {
      changeConfig,
      discoverInferenceModels,
      probeInferenceEndpoint,
      removeProviderAccount,
    },
  } = useApp();
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
    connectTimeoutSecs: str(backend.connectTimeoutSecs),
    discoveryTimeoutSecs: str(backend.discoveryTimeoutSecs),
    maxConcurrent: str(backend.maxConcurrent),
    maxQueueDepth: str(backend.maxQueueDepth),
    enabled: backend.enabled ?? true,
    tags: backend.tags,
  };
  /* each field's value as the patch takes it; each throws its problem */
  const fieldsOf = (next: typeof saved) => ({
    name: () => {
      const name = next.name.trim();
      if (!name) throw new Error("Name is required");
      return name;
    },
    endpoint: () =>
      next.providerKind === "ClaudeCliSubscription" &&
      next.endpoint === "claude-cli://subscription"
        ? next.endpoint
        : requiredHttpUrl("Endpoint", next.endpoint),
    apiKeyEnvVar: () => {
      if (next.apiKey.trim() && next.apiKeyEnvVar.trim())
        throw new Error("Choose an API key or an environment variable, not both");
    },
    maxConcurrent: () =>
      optionalInteger("Max concurrent", next.maxConcurrent, { min: 1 }),
    maxQueueDepth: () =>
      optionalInteger("Max queue depth", next.maxQueueDepth, { min: 0 }),
    connectTimeoutSecs: () =>
      optionalInteger("Connect timeout seconds", next.connectTimeoutSecs, { min: 1 }),
    discoveryTimeoutSecs: () =>
      optionalInteger("Discovery timeout seconds", next.discoveryTimeoutSecs, {
        min: 1,
      }),
  });
  const d = useDraft(
    saved,
    async (next) => {
      const value = fieldsOf(next);
      const name = value.name();
      const endpoint = value.endpoint();
      const maxConcurrent = value.maxConcurrent();
      const maxQueueDepth = value.maxQueueDepth();
      const connectTimeoutSecs = value.connectTimeoutSecs();
      const discoveryTimeoutSecs = value.discoveryTimeoutSecs();
      let auth: InferenceBackend["auth"] | undefined;
      if (isSubscriptionKind(next.providerKind))
        auth = {
          kind: "principal_oauth",
          ...(backend.accountRef ? { account_ref: backend.accountRef } : {}),
        };
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
        tags: next.tags.length ? next.tags : null,
      };
      if (auth) changes.auth = auth;
      await changeConfig("patchConfigComponents", {
        agentDid: deployment.agentDid,
        patches: [{ collection: "InferenceBackend", id: backend.backendId, changes }],
      });
    },
    {
      problems: (next) => {
        const fields = fieldsOf(next);
        return Object.fromEntries(
          (Object.keys(fields) as (keyof typeof fields)[]).map((field) => [
            field,
            problemOf(fields[field]),
          ]),
        );
      },
    },
  );
  const [probe, setProbe] = useState<string | null>(null);
  const [discoveredModels, setDiscoveredModels] = useState<string[] | null>(null);
  const [discoveries] = useState(newestWins);
  const id = (f: string) => `${backend.backendId}-${f}`;
  const subscription = d.draft.providerKind in SUBSCRIPTION;
  useEffect(() => {
    discoveries.supersede();
    setDiscoveredModels(null);
    setProbe(null);
  }, [d.draft.providerKind, d.draft.endpoint]);
  /* an added account's backend is deleted by removing the account */
  const removable = backend.accountRef ? backendAccount(accounts, backend) : undefined;
  const signIn = SUBSCRIPTION[backend.providerKind ?? ""];
  /* a disabled or missing account draws no usage */
  const usable = !signIn || backendAccount(accounts, backend)?.enabled;
  const [refreshing, setRefreshing] = useState(false);
  const refreshUsage = async () => {
    setRefreshing(true);
    try {
      await usage?.refresh(signIn?.provider ?? null);
    } catch (error) {
      toast(`Refresh failed: ${setupErrorMessage(error)}`);
    } finally {
      setRefreshing(false);
    }
  };
  const users = deployment.inferenceProfiles
    .filter((p) => p.backend_id === backend.backendId)
    .map((p) => p.display_name ?? p.profile_id);
  return (
    <>
      <Group
        title={embedded ? undefined : (backend.name ?? backend.backendId)}
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
                const current = discoveries.begin();
                try {
                  if (subscription) {
                    setProbe("Discovering models…");
                    const connection = SUBSCRIPTION[d.draft.providerKind]!;
                    const result = await discoverInferenceModels({
                      requestKey: `backend-${backend.backendId}-${Date.now()}`,
                      agentDid: deployment.agentDid,
                      provider: connection.providerId,
                      authMethod: connection.authMethod,
                      endpoint: d.draft.endpoint,
                      apiKey: null,
                      accountRef: backend.accountRef ?? null,
                    });
                    if (!result.reachable)
                      throw new Error(result.failure?.message ?? "Discovery failed");
                    if (!current()) return;
                    setDiscoveredModels(
                      result.models.map((option) => option.advertised.model_name),
                    );
                    setProbe(`Authenticated · ${result.models.length} models`);
                    return;
                  }
                  const endpoint = requiredHttpUrl("Endpoint", d.draft.endpoint);
                  setProbe("probing…");
                  const r = await probeInferenceEndpoint(endpoint);
                  if (!current()) return;
                  setProbe(
                    r.reachable
                      ? `reachable · ${r.models.length} models`
                      : "unreachable",
                  );
                  toast(r.reachable ? "Endpoint reachable" : "Endpoint unreachable");
                } catch (error) {
                  if (!current()) return;
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
          error={d.problems.name}
          onChange={(v) => d.set("name", v)}
        />
        <ChoiceRow
          id={id("kind")}
          label="Provider kind"
          description="Decides how it is paid for: a key, or a subscription sign-in."
          value={d.draft.providerKind}
          onChange={(v) => d.set("providerKind", v)}
          items={KINDS}
        />
        {!subscription && (
          <ChoiceRow
            id={id("wire")}
            label="OpenAI wire API"
            description="Leave automatic unless the endpoint requires one protocol."
            value={d.draft.openaiWireApi}
            onChange={(v) => d.set("openaiWireApi", v)}
            items={[
              { value: "responses", label: "Responses" },
              { value: "chat_completions", label: "Chat completions" },
            ]}
            none="Automatic"
          />
        )}
        <FactRow
          label="Used by"
          description="Delete is blocked while a behavior points here."
        >
          {users.length ? users.join(", ") : "no behavior"}
        </FactRow>
      </Group>
      <Group title={subscription ? "Subscription" : "Credential"}>
        {subscription ? (
          <AccountRows
            deployment={deployment}
            kind={d.draft.providerKind}
            accountRef={backend.accountRef ?? null}
            accounts={accounts}
          />
        ) : (
          <>
            <TextRow
              id={id("env")}
              label="API key env var"
              description="Read from the environment at start."
              value={d.draft.apiKeyEnvVar}
              error={d.problems.apiKeyEnvVar}
              onChange={(v) => d.set("apiKeyEnvVar", v)}
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
              placeholder={backend.apiKeyConfigured ? "Configured" : "sk-…"}
              password
            />
            {backend.apiKeyConfigured && (
              <Row label="Stored key">
                <Button
                  size="sm"
                  variant="quiet"
                  onClick={() =>
                    changeConfig(
                      "patchConfigComponents",
                      {
                        agentDid: deployment.agentDid,
                        patches: [
                          {
                            collection: "InferenceBackend",
                            id: backend.backendId,
                            changes: { auth: { kind: "unauthenticated" } },
                          },
                        ],
                      },
                      "clear the stored key",
                    )
                      .then(() => toast("Stored key cleared"))
                      .catch((error) => toastFailure("clear the stored key", error))
                  }
                >
                  Clear stored key
                </Button>
              </Row>
            )}
          </>
        )}
      </Group>
      {usage && usable && (
        <Group
          title="Usage"
          action={
            <Button
              size="sm"
              variant="outline"
              disabled={refreshing}
              onClick={refreshUsage}
            >
              Refresh
            </Button>
          }
        >
          <UsageRows view={usage.view} />
        </Group>
      )}
      <Group title="Endpoint and models">
        <TextRow
          id={id("endpoint")}
          label="Endpoint"
          value={d.draft.endpoint}
          error={d.problems.endpoint}
          onChange={(v) => d.set("endpoint", v)}
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
          error={d.problems.connectTimeoutSecs}
          placeholder="Runtime default (10)"
          onChange={(v) => d.set("connectTimeoutSecs", v)}
        />
        <NumberRow
          id={id("discovery-timeout")}
          label="Discovery timeout seconds"
          description="Positive whole number, or blank for the runtime default."
          value={d.draft.discoveryTimeoutSecs}
          error={d.problems.discoveryTimeoutSecs}
          placeholder="Runtime default (10)"
          onChange={(v) => d.set("discoveryTimeoutSecs", v)}
        />
        <NumberRow
          id={id("conc")}
          label="Max concurrent"
          description="Whole number of 1 or more."
          value={d.draft.maxConcurrent}
          error={d.problems.maxConcurrent}
          onChange={(v) => d.set("maxConcurrent", v)}
        />
        <NumberRow
          id={id("queue")}
          label="Max queue depth"
          description="Whole number of 0 or more; 0 disables queueing."
          value={d.draft.maxQueueDepth}
          error={d.problems.maxQueueDepth}
          onChange={(v) => d.set("maxQueueDepth", v)}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.set("enabled", v)}
        />
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        draft={d}
        fields={{
          name: id("name"),
          apiKeyEnvVar: id("env"),
          endpoint: id("endpoint"),
          connectTimeoutSecs: id("connect-timeout"),
          discoveryTimeoutSecs: id("discovery-timeout"),
          maxConcurrent: id("conc"),
          maxQueueDepth: id("queue"),
        }}
      />
      {!embedded && removable && (
        <DeleteButton
          label={removable.label}
          noun="account"
          warning={accountWarnings(deployment, accounts, removable).remove}
          base={base}
          onDelete={() =>
            removeProviderAccount(deployment.agentDid, removable.credentialId)
          }
        />
      )}
      {!embedded && !removable && (
        <DeleteButton
          label={backend.name ?? backend.backendId}
          warning={dependentsWarning(deployment, "backend", backend.backendId)}
          base={base}
          onDelete={() =>
            changeConfig("deleteBackendConfig", {
              backendId: backend.backendId,
              agentDid: deployment.agentDid,
            })
          }
        />
      )}
    </>
  );
}
