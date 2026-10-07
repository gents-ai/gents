/* Inference: backends and the provider accounts that some of them need,
   in one list. A row is a backend; its kind decides the credential
   section. OpenAI-compatible and OpenRouter take a key or an env var;
   ChatGPT/Codex and Grok exist only through a subscription sign-in, so
   the account card sits in the row with connect, cancel and disconnect. */
import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "./enabled";
import { dependentsWarning } from "./dependents";
import { useCallback, useEffect, useRef, useState } from "react";
import { PROVIDER_VISUALS, SetupScreen, type ProviderId } from "../setup/SetupScreen";
import type { InferenceProviderOption } from "@source-inc/gents-desktop-client";
import { toast } from "sonner";
import type {
  DesktopApiAdapter,
  BackendProviderKind,
  BackendSaveRequest,
  BackendUsageView,
  InferenceBackend,
  InferenceBackendView,
  OpenAiWireApi,
  ProviderAccountView,
  InferenceAuthMethod,
  InferenceProviderId,
  UsageWindowView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import {
  bridgeErrorCode,
  CREDENTIAL_NOT_SAVED,
  PROVIDER_CREDENTIAL_KIND,
  setupErrorMessage,
} from "@/lib/providerLogin";
import { optionalInteger, requiredHttpUrl, str, useDraft } from "./draft";
import {
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  SwitchRow,
  TextRow,
  TagsRow,
} from "./editors";
import { ConfirmDelete, DeleteButton, ListDetail, type ListRow } from "./ListDetail";
import { Group, Row } from "./rows";
import { RowMenu } from "./RowMenu";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { Plus } from "lucide-react";
import { ProviderLogo } from "../ProviderLogo";
import { RenameDialog } from "../AgentsScreen";
import { useApp } from "@/app/AppContext";
import { toastFailure } from "@/lib/failure";

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
  { value: "AnthropicApiKey", label: "Anthropic API key" },
];
/* subscription kinds, and the provider name their account carries */
const SUBSCRIPTION: Record<
  string,
  {
    provider: string;
    providerId: InferenceProviderId;
    authMethod: InferenceAuthMethod;
    title: string;
    note: string;
    login: "codex" | "grok" | "claude";
  }
> = {
  ChatGptCodex: {
    provider: PROVIDER_CREDENTIAL_KIND.openai,
    providerId: "openai",
    authMethod: "chat_gpt_oauth",
    title: "ChatGPT / Codex",
    note: "Use an eligible ChatGPT subscription for Codex inference.",
    login: "codex",
  },
  XaiGrokOAuth: {
    provider: PROVIDER_CREDENTIAL_KIND.grok,
    providerId: "grok",
    authMethod: "grok_oauth",
    title: "Grok / xAI",
    note: "Use SuperGrok or an eligible X Premium+ subscription.",
    login: "grok",
  },
  ClaudeCliSubscription: {
    provider: PROVIDER_CREDENTIAL_KIND.anthropic,
    providerId: "anthropic",
    authMethod: "claude_oauth",
    title: "Anthropic / Claude",
    note: "Use a Claude Pro or Max subscription.",
    login: "claude",
  },
};

const NO_ACCOUNTS: ProviderAccountView[] = [];

export function useAccounts(agentDid: string) {
  const { api, stores } = useApp();
  const snapshot = stores.client.use.snapshot();
  /* held with the agent they were read for: another agent's never show */
  const [held, setHeld] = useState<{
    agentDid: string;
    views: ProviderAccountView[];
  } | null>(null);
  const accounts = held?.agentDid === agentDid ? held.views : NO_ACCOUNTS;
  const latest = useRef(0);
  const load = useCallback(() => {
    const read = ++latest.current;
    return (api.listProviderAccounts?.(agentDid) ?? Promise.resolve([])).then(
      (views) => {
        if (latest.current === read) setHeld({ agentDid, views });
      },
      () => {
        if (latest.current === read) setHeld({ agentDid, views: [] });
      },
    );
  }, [api, agentDid]);
  useEffect(() => {
    void load();
    return () => {
      latest.current += 1;
    };
  }, [load, snapshot]);
  return { accounts, reload: load };
}

/* usage, read when the panel opens and on Refresh (both skip accounts read
   in the last five minutes); nothing polls, and a snapshot change does not
   read again */
function useProviderUsage(agentDid: string) {
  const { api } = useApp();
  const [usage, setUsage] = useState<BackendUsageView[]>([]);
  /* only the latest read draws: an older one, or another agent's, may land later */
  const latest = useRef(0);
  useEffect(() => {
    const read = ++latest.current;
    setUsage([]);
    api.readProviderUsage?.(agentDid, false, null).then(
      (views) => {
        if (latest.current === read) setUsage(views);
      },
      () => undefined,
    );
    return () => {
      latest.current += 1;
    };
  }, [api, agentDid]);
  const refresh = async (provider: string | null) => {
    const read = ++latest.current;
    const views = await api.readProviderUsage?.(agentDid, true, provider);
    if (views && latest.current === read) setUsage(views);
  };
  return { usage, refresh };
}

/* "2h13m", "3m", "<1m": the CLI's short durations */
function shortDuration(ms: number) {
  const total = Math.max(0, Math.floor(ms / 60_000));
  const [days, hours, minutes] = [
    Math.floor(total / 1440),
    Math.floor((total % 1440) / 60),
    total % 60,
  ];
  if (days) return hours ? `${days}d${hours}h` : `${days}d`;
  if (hours) return minutes ? `${hours}h${minutes}m` : `${hours}h`;
  return minutes ? `${minutes}m` : "<1m";
}

const USAGE_SOURCE: Record<string, string> = {
  header: "from response headers",
  endpoint: "from the usage endpoint",
  error: "from a rejected request",
};

function windowText(w: UsageWindowView, now: number) {
  const parts = [`${Math.round(w.usedPct)}% used`];
  if (w.resetsAt) {
    const at = new Date(w.resetsAt);
    const time = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    parts.push(`resets in ${shortDuration(at.getTime() - now)} (${time})`);
  }
  parts.push(
    `${USAGE_SOURCE[w.source] ?? w.source}, ${shortDuration(now - Date.parse(w.observedAt))} ago`,
  );
  if (w.lastKnown) parts.push("last known");
  return parts.join(" · ");
}

/* a row's usage: its most-used window, labelled, the one that blocks first */
function UsageBar({ view }: { view?: BackendUsageView }) {
  const top = view?.windows.reduce<UsageWindowView | undefined>(
    (most, w) => (!most || w.usedPct > most.usedPct ? w : most),
    undefined,
  );
  if (!top) return null;
  const pct = Math.round(top.usedPct);
  return (
    <span className="flex items-center gap-1.5 px-1 text-xs text-muted-foreground tabular-nums">
      {top.label}{" "}
      <span aria-hidden className="h-1.5 w-12 overflow-hidden rounded-full bg-muted">
        <span
          className="block h-full bg-foreground/60"
          style={{ width: `${Math.min(pct, 100)}%` }}
        />
      </span>
      {pct}%
    </span>
  );
}

/* the opened row's usage: each window, or why there is no number, and how
   this read went */
function UsageRows({ view }: { view?: BackendUsageView }) {
  const now = Date.now();
  const read = view?.read?.startsWith("unavailable: ")
    ? `Not read: ${view.read.slice("unavailable: ".length)}`
    : view?.readError;
  return (
    <>
      {view?.windows.length ? (
        view.windows.map((w) => (
          <FactRow key={w.label} label={w.label}>
            {windowText(w, now)}
          </FactRow>
        ))
      ) : (
        <FactRow label="Reported">{view?.note ?? "unknown"}</FactRow>
      )}
      {read && <FactRow label="Last read">{read}</FactRow>}
    </>
  );
}

/* the stored account a subscription backend runs on: the one with the
   backend's reference; no reference is the provider's original account */
function referencedAccount(
  accounts: ProviderAccountView[],
  provider: string,
  accountRef: string | null | undefined,
) {
  return accounts.find(
    (a) =>
      a.provider === provider &&
      !a.pendingSave &&
      (a.accountRef ?? null) === (accountRef ?? null),
  );
}

/* the account a subscription backend runs on, if it is stored here */
function backendAccount(accounts: ProviderAccountView[], b: InferenceBackendView) {
  const sub = SUBSCRIPTION[b.providerKind ?? ""];
  return sub ? referencedAccount(accounts, sub.provider, b.accountRef) : undefined;
}

/* a backend as a profile sees it: a subscription backend serves only on its
   enabled account and is named "<label> · <provider>"; others always serve */
export function profileBackend(
  accounts: ProviderAccountView[],
  b: InferenceBackendView,
) {
  const sub = SUBSCRIPTION[b.providerKind ?? ""];
  const account = backendAccount(accounts, b);
  return {
    provider: sub?.provider,
    account,
    usable: !sub || Boolean(account?.enabled),
    label: sub && account ? `${account.label} · ${sub.title}` : (b.name ?? b.backendId),
  };
}

/* what an account action leaves behind: the profiles that fail their next
   turn, in the CLI's words, and for remove which backends go with it (an
   added account's backends that no profile uses, as the CLI's remove) */
function accountWarnings(
  deployment: NodeView,
  accounts: ProviderAccountView[],
  account: ProviderAccountView,
) {
  const backends = deployment.inferenceBackends.filter(
    (b) => backendAccount(accounts, b)?.credentialId === account.credentialId,
  );
  const usedBy = (b: InferenceBackendView) =>
    deployment.inferenceProfiles.filter((p) => p.backend_id === b.backendId);
  const profiles = backends.flatMap(usedBy).map((p) => p.display_name ?? p.profile_id);
  const names = (list: InferenceBackendView[]) =>
    list.map((b) => b.name ?? b.backendId).join(", ");
  const gone = account.accountRef ? backends.filter((b) => !usedBy(b).length) : [];
  const kept = backends.filter((b) => !gone.includes(b));
  const disconnect = profiles.length
    ? `These profiles use this account and fail their next turn until moved to another backend: ${profiles.join(", ")}.`
    : "";
  const remove = [
    disconnect,
    gone.length ? `Deletes its unused backends: ${names(gone)}.` : "",
    kept.length
      ? `${account.accountRef ? "Keeps the backends a profile uses" : "Keeps its backends"}: ${names(kept)}.`
      : "",
  ]
    .filter(Boolean)
    .join(" ");
  return { disconnect, remove };
}

type AccountAction = {
  action: "rename" | "disconnect" | "remove";
  account: ProviderAccountView;
};

/* rename, disconnect and remove for an account, opened from its row's menu;
   none of them edits a profile or a backend */
function AccountDialogs({
  deployment,
  accounts,
  reload,
  acting,
  onClose,
}: {
  deployment: NodeView;
  accounts: ProviderAccountView[];
  reload: () => Promise<void>;
  acting: AccountAction | null;
  onClose: () => void;
}) {
  const { api } = useApp();
  const [busy, setBusy] = useState(false);
  const account = acting?.account;
  const warnings = account && accountWarnings(deployment, accounts, account);
  const rename = async (label: string) => {
    if (!account) return;
    if (
      accounts.some(
        (a) =>
          a.provider === account.provider &&
          !a.pendingSave &&
          a.credentialId !== account.credentialId &&
          a.label === label,
      )
    )
      throw new Error(
        `Another account is already labelled “${label}”. Choose another label.`,
      );
    await api.renameProviderAccount?.(deployment.agentDid, account.credentialId, label);
    toast("Renamed");
    await reload();
  };
  const disconnect = async () => {
    if (!account) return;
    setBusy(true);
    try {
      await api.disconnectProviderAccount?.(deployment.agentDid, account.credentialId);
      toast("Disconnected");
      onClose();
      await reload();
    } catch (error) {
      toast(`Disconnect failed: ${setupErrorMessage(error)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <RenameDialog
        key={account?.credentialId ?? "none"}
        title="Rename account"
        description="The label this account goes by on this agent."
        value={acting?.action === "rename" ? account!.label : null}
        onSave={rename}
        onClose={onClose}
      />
      <Dialog
        open={acting?.action === "disconnect"}
        onOpenChange={(open) => !open && !busy && onClose()}
      >
        <DialogContent aria-modal="true">
          <DialogHeader>
            <DialogTitle>Disconnect {account?.label}?</DialogTitle>
            <DialogDescription>
              Signing in to it again reconnects it. {warnings?.disconnect}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={onClose}>
              Keep connected
            </Button>
            <Button variant="destructive" disabled={busy} onClick={disconnect}>
              {busy ? "Disconnecting…" : "Disconnect now"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {account && (
        <ConfirmDelete
          label={account.label}
          noun="account"
          open={acting?.action === "remove"}
          onOpenChange={(open) => !open && onClose()}
          onDelete={() => removeAccount(api, deployment, account, reload)}
          warning={warnings?.remove}
        />
      )}
    </>
  );
}

async function removeAccount(
  api: DesktopApiAdapter,
  deployment: NodeView,
  account: ProviderAccountView,
  reload: () => Promise<void>,
) {
  await api.removeProviderAccount?.(deployment.agentDid, account.credentialId);
  await reload();
}

/* the account card for a subscription backend */
function AccountRows({
  deployment,
  kind,
  accountRef,
  accounts,
  reload,
}: {
  deployment: NodeView;
  kind: string;
  accountRef: string | null;
  accounts: ProviderAccountView[];
  reload: () => Promise<void>;
}) {
  const { api } = useApp();
  const sub = SUBSCRIPTION[kind]!;
  const stored = referencedAccount(accounts, sub.provider, accountRef);
  const account = stored?.enabled ? stored : undefined;
  /* signing in here would add a new account, never this one */
  const elsewhere = accountRef !== null && !stored;
  const unsaved = accounts.some((a) => a.provider === sub.provider && a.pendingSave);
  const [busy, setBusy] = useState(false);
  const [confirmingDisconnect, setConfirmingDisconnect] = useState(false);
  const signIn = async () => {
    setBusy(true);
    try {
      if (sub.login === "codex") await api.codexLogin(deployment.agentDid);
      else if (sub.login === "claude") await api.claudeLogin(deployment.agentDid);
      else await api.grokLogin(deployment.agentDid);
      toast("Signed in");
      await reload();
    } catch (error) {
      toast(`Sign in failed: ${setupErrorMessage(error)}`);
      if (bridgeErrorCode(error) === CREDENTIAL_NOT_SAVED) await reload();
    } finally {
      setBusy(false);
    }
  };
  const retrySave = async () => {
    if (!api.retrySaveProviderAccount) return;
    setBusy(true);
    try {
      await api.retrySaveProviderAccount(deployment.agentDid, sub.provider);
      toast("Signed in");
      await reload();
    } catch (error) {
      toast(`Save failed: ${setupErrorMessage(error)}`);
      await reload();
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
  if (elsewhere)
    return (
      <Row label="Account" description={sub.note}>
        <span className="text-sm text-muted-foreground">account not on this node</span>
      </Row>
    );
  return (
    <>
      <Row label="Account" description={sub.note}>
        <span className="flex items-center gap-2">
          {stored && (
            <Badge variant="secondary">{account ? "Connected" : "Disabled"}</Badge>
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
          {unsaved && api.retrySaveProviderAccount ? (
            <Button size="sm" variant="brand" disabled={busy} onClick={retrySave}>
              Retry save
            </Button>
          ) : null}
          <Button
            size="sm"
            variant={stored || unsaved ? "outline" : "brand"}
            disabled={busy}
            onClick={signIn}
          >
            {busy ? "Signing in…" : stored ? "Reconnect" : "Connect"}
          </Button>
        </span>
      </Row>
      {stored && (
        <>
          <FactRow label="Signed in as">
            {stored.accountId ?? "Account identity unavailable — reconnect to refresh"}
            {stored.planType ? ` · ${stored.planType}` : ""}
          </FactRow>
          <FactRow label="Label">{stored.label}</FactRow>
        </>
      )}
    </>
  );
}

export function BackendEditor({
  deployment,
  backend,
  accounts,
  reload,
  usage,
  embedded = false,
}: {
  deployment: NodeView;
  backend: InferenceBackendView;
  accounts: ProviderAccountView[];
  reload: () => Promise<void>;
  /* this backend's usage and the read again; absent, no Usage group */
  usage?: {
    view?: BackendUsageView;
    refresh: (provider: string | null) => Promise<void>;
  };
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const {
    api,
    actions: { changeConfig },
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
  });
  const [probe, setProbe] = useState<string | null>(null);
  const [discoveredModels, setDiscoveredModels] = useState<string[] | null>(null);
  const discoveryRevision = useRef(0);
  const id = (f: string) => `${backend.backendId}-${f}`;
  const subscription = d.draft.providerKind in SUBSCRIPTION;
  useEffect(() => {
    discoveryRevision.current += 1;
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
                const revision = ++discoveryRevision.current;
                try {
                  if (subscription) {
                    setProbe("Discovering models…");
                    const connection = SUBSCRIPTION[d.draft.providerKind]!;
                    const result = await api.discoverInferenceModels({
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
                    if (discoveryRevision.current !== revision) return;
                    setDiscoveredModels(
                      result.models.map((option) => option.advertised.model_name),
                    );
                    setProbe(`Authenticated · ${result.models.length} models`);
                    return;
                  }
                  const endpoint = requiredHttpUrl("Endpoint", d.draft.endpoint);
                  setProbe("probing…");
                  const r = await api.probeInferenceEndpoint(endpoint);
                  if (discoveryRevision.current !== revision) return;
                  setProbe(
                    r.reachable
                      ? `reachable · ${r.models.length} models`
                      : "unreachable",
                  );
                  toast(r.reachable ? "Endpoint reachable" : "Endpoint unreachable");
                } catch (error) {
                  if (discoveryRevision.current !== revision) return;
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
          placeholder="Runtime default (10)"
          onChange={(v) => d.set("connectTimeoutSecs", v)}
        />
        <NumberRow
          id={id("discovery-timeout")}
          label="Discovery timeout seconds"
          description="Positive whole number, or blank for the runtime default."
          value={d.draft.discoveryTimeoutSecs}
          placeholder="Runtime default (10)"
          onChange={(v) => d.set("discoveryTimeoutSecs", v)}
        />
        <NumberRow
          id={id("conc")}
          label="Max concurrent"
          description="Whole number of 1 or more."
          value={d.draft.maxConcurrent}
          onChange={(v) => d.set("maxConcurrent", v)}
        />
        <NumberRow
          id={id("queue")}
          label="Max queue depth"
          description="Whole number of 0 or more; 0 disables queueing."
          value={d.draft.maxQueueDepth}
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
      <DraftActions draft={d} />
      {!embedded && removable && (
        <DeleteButton
          label={removable.label}
          noun="account"
          warning={accountWarnings(deployment, accounts, removable).remove}
          base={base}
          onDelete={() => removeAccount(api, deployment, removable, reload)}
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

/* the provider catalog the bridge publishes, once per mount */
/* the provider catalog; a failed read is said, with a way to ask again */
function useSetupCatalog() {
  const { api } = useApp();
  const [providers, setProviders] = useState<InferenceProviderOption[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setError(null);
    const read = api.getInferenceSetupCatalog?.();
    if (!read) return;
    read.then(
      (c) => {
        if (live) setProviders(c.providers);
      },
      (e: unknown) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      },
    );
    return () => {
      live = false;
    };
  }, [api, attempt]);
  return { providers, error, retry: () => setAttempt((n) => n + 1) };
}

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
    agentDid: deployment.agentDid,
    section: "inference",
  };
  const models = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "profiles",
  };
  const { accounts, reload } = useAccounts(deployment.agentDid);
  const [acting, setActing] = useState<AccountAction | null>(null);
  const providerUsage = useProviderUsage(deployment.agentDid);
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
        agentDid={deployment.agentDid}
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
                        deployment.agentDid,
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
                            agentDid: deployment.agentDid,
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
              reload={reload}
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
        reload={reload}
        acting={acting}
        onClose={() => setActing(null)}
      />
    </>
  );
}
