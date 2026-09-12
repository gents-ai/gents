/* Inference: backends and the provider accounts that some of them need,
   in one list. A row is a backend; its kind decides the credential
   section. OpenAI-compatible and OpenRouter take a key or an env var;
   ChatGPT/Codex and Grok exist only through a subscription sign-in, so
   the account card sits in the row with connect, cancel and disconnect.
   Everything else is the desktop app's Backends panel field for field. */
import { useEffect, useState } from "react";
import { toast } from "sonner";
import type {
  BackendProviderKind,
  BackendSaveRequest,
  DeploymentView,
  InferenceBackendView,
  OpenAiWireApi,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import { intOrNull, newId, str, toLines, useDraft } from "./draft";
import { AreaRow, ChoiceRow, FactRow, NumberRow, SwitchRow, TextRow } from "./editors";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group, Row } from "./rows";
import { ProviderLogo } from "../ProviderLogo";

function backendSave(
  agentDid: string,
  fields: {
    backendId: string;
    name: string;
    providerKind: string;
    openaiWireApi: string | null;
    endpoint: string;
    apiKey: string | null;
    apiKeyEnvVar: string | null;
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
      auth: fields.apiKey
        ? { kind: "api_key", key: fields.apiKey }
        : fields.apiKeyEnvVar
          ? { kind: "environment", variable: fields.apiKeyEnvVar }
          : { kind: "unauthenticated" },
      max_concurrent: fields.maxConcurrent,
      max_queue_depth: fields.maxQueueDepth,
      enabled: fields.enabled,
    },
  };
}

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
    provider: "chatgpt",
    title: "ChatGPT / Codex",
    note: "Use an eligible ChatGPT subscription for Codex inference.",
    login: "codex",
  },
  XaiGrokOAuth: {
    provider: "xai",
    title: "Grok / xAI",
    note: "Use SuperGrok or an eligible X Premium+ subscription.",
    login: "grok",
  },
  ClaudeCliSubscription: {
    provider: "claude-subscription",
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
  const account = accounts.find((a) => a.provider === sub.provider);
  const [openedAt] = useState(() => Date.now());
  const expired = account ? Date.parse(account.accessTokenExpiresAt) < openedAt : false;
  const [busy, setBusy] = useState(false);
  const api = shell.api;
  const signIn = async () => {
    setBusy(true);
    try {
      if (sub.login === "codex") await api.codexLogin(deployment.agentDid);
      else if (sub.login === "claude") await api.claudeLogin(deployment.agentDid);
      else await api.grokLogin(deployment.agentDid);
      toast("Signed in");
      await reload();
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
          {account && (
            <Button
              size="sm"
              variant="quiet"
              disabled={busy}
              onClick={async () => {
                if (!confirm(`Disconnect ${sub.title}?`)) return;
                await api.disconnectProviderAccount?.(
                  deployment.agentDid,
                  account.credentialId,
                );
                toast("Disconnected");
                await reload();
              }}
            >
              Disconnect
            </Button>
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
            {account.accountId ?? "—"}
            {account.planType ? ` · ${account.planType}` : ""}
          </FactRow>
          <FactRow label="Credential" mono>
            {account.credentialId}
          </FactRow>
          <FactRow label="Expires">
            {new Date(account.accessTokenExpiresAt).toLocaleString()}
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
    endpoint: backend.endpoint ?? "",
    models: toLines(backend.models),
    apiKeyEnvVar: backend.apiKeyEnvVar ?? "",
    apiKey: "",
    maxConcurrent: str(backend.maxConcurrent),
    maxQueueDepth: str(backend.maxQueueDepth),
    enabled: backend.enabled ?? true,
  };
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveBackendConfig(
        backendSave(deployment.agentDid, {
          backendId: backend.backendId,
          name: next.name.trim() || backend.backendId,
          providerKind: next.providerKind,
          openaiWireApi: backend.openaiWireApi,
          endpoint: next.endpoint,
          apiKey: next.apiKey || null,
          apiKeyEnvVar: next.apiKeyEnvVar || null,
          maxConcurrent: intOrNull(next.maxConcurrent),
          maxQueueDepth: intOrNull(next.maxQueueDepth),
          enabled: next.enabled,
        }),
      ),
    ),
  );
  const [probe, setProbe] = useState<string | null>(null);
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
            <Badge variant={backend.probeStatus === "ok" ? "secondary" : "destructive"}>
              {backend.probeStatus ?? "unprobed"}
            </Badge>
            <Button
              size="sm"
              variant="outline"
              onClick={async () => {
                setProbe("probing…");
                const r = await shell.api
                  .probeInferenceEndpoint(d.draft.endpoint)
                  .catch(() => null);
                setProbe(
                  r
                    ? r.reachable
                      ? `reachable · ${r.models.length} models`
                      : "unreachable"
                    : "probe failed",
                );
                toast(r?.reachable ? "Endpoint reachable" : "Endpoint unreachable");
              }}
            >
              Probe
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
                        api.saveBackendConfig(
                          backendSave(deployment.agentDid, {
                            backendId: backend.backendId,
                            name: backend.name ?? backend.backendId,
                            providerKind: backend.providerKind ?? "OpenAiCompatible",
                            openaiWireApi: backend.openaiWireApi,
                            endpoint: backend.endpoint ?? "",
                            apiKey: null,
                            apiKeyEnvVar: backend.apiKeyEnvVar,
                            maxConcurrent: backend.maxConcurrent,
                            maxQueueDepth: backend.maxQueueDepth,
                            enabled: backend.enabled,
                          }),
                        ),
                      )
                      .then(() => toast("Stored key cleared"))
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
        <AreaRow
          id={id("models")}
          label="Models"
          description="One per line; at least one."
          value={d.draft.models}
          onChange={(v) => d.set("models", v)}
          onCommit={d.commit}
          rows={3}
          mono
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
          description="Whole number of 1 or more."
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
      </Group>
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
    return `${b.models[0] ?? "no models"} · ${KINDS.find((k) => k.value === b.providerKind)?.label ?? b.providerKind} · ${cred}`;
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
        badgeTone: b.probeStatus === "ok" ? "default" : "bad",
      }))}
      createLabel="New backend"
      empty="No inference yet. Add a backend: a local server, a key, or a subscription."
      onCreate={async () => {
        const backendId = newId("backend");
        await shell.applyConfig((api) =>
          api.saveBackendConfig(
            backendSave(deployment.agentDid, {
              backendId,
              name: "New backend",
              providerKind: "OpenAiCompatible",
              openaiWireApi: "chat_completions",
              endpoint: "",
              apiKey: null,
              apiKeyEnvVar: null,
              maxConcurrent: 2,
              maxQueueDepth: 8,
              enabled: true,
            }),
          ),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "inference",
          item: backendId,
        });
      }}
      detail={(id) => {
        const backend = deployment.inferenceBackends.find((b) => b.backendId === id)!;
        return (
          <Editor
            key={JSON.stringify(backend)}
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
