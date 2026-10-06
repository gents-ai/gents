/* The inference step: choose a provider, connect or sign in, pick a model
   and its defaults, and save them in one operator transaction. Shared by
   first run, setup re-entry and adding a backend from the agent screen.
   Provider/model guidance comes from the versioned Rust contract. */
import { useEffect, useRef, useState } from "react";
import {
  ArrowRight,
  CircleCheck,
  KeyRound,
  Orbit,
  Server,
  Sparkles,
} from "lucide-react";
import type {
  DesktopClientSnapshot,
  InferenceAuthMethod,
  InferenceDiscoveryResult,
  InferenceModelOption,
  InferenceModelRecommendation,
  InferenceProviderId,
  InferenceSetupCatalog,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { type ManagedServerWait } from "../../../lib/managedServerStartup";
import { ManagedServerWaitNotice } from "./SetupProgress";
import { supportsLocalManagedServer } from "../../../lib/shellPlatform";
import { openExternalUrl } from "../../../lib/externalLinks";
import {
  bridgeErrorCode,
  CREDENTIAL_NOT_SAVED,
  setupErrorMessage,
  watchProviderLoginUrl,
  PROVIDER_CREDENTIAL_KIND,
  type OauthProvider,
} from "@/lib/providerLogin";
import { isLocalAgent } from "@/lib/firstRun";
import { ensureManagedRuntimeServing } from "@/lib/managedRuntimeReadiness";
import {
  currentInferenceDiscovery,
  inferenceDiscoveryKey,
} from "@/lib/providerDiscovery";
import { buildInferenceSetupPlan } from "@/lib/inferenceSetupPersistence";
import {
  InferenceModelControls,
  recommendedInferenceSettings,
  validateInferenceSettings,
  type InferenceSettingsDraft,
} from "../inference/InferenceModelControls";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useSelectedNode } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf } from "../../../hooks/fleetStore";
import { Field, Frame, Nav, Option, Title } from "./parts";

/* The runtime confirms a save only after it reconciles the new documents,
   starts the rebound behavior and replicates its readiness back. */
const SAVE_CONFIRMATION_TIMEOUT_MS = 15_000;

export type ProviderId = InferenceProviderId;
type ConnectionDraft = {
  authMethod: InferenceAuthMethod;
  endpoint: string;
  apiKey: string;
};

export const PROVIDER_VISUALS: Record<
  ProviderId,
  { icon: typeof Server; logo: string }
> = {
  openai: { icon: KeyRound, logo: "/logos/openai.svg" },
  anthropic: { icon: Sparkles, logo: "/logos/claude.svg" },
  grok: { icon: Orbit, logo: "/logos/grok.svg" },
  local: { icon: Server, logo: "/logos/ollama.svg" },
  openrouter: { icon: KeyRound, logo: "/logos/openrouter.svg" },
};

const authLabel = (method: InferenceAuthMethod) =>
  ({
    chat_gpt_oauth: "ChatGPT sign-in",
    api_key: "API key",
    claude_oauth: "Claude sign-in",
    grok_oauth: "Grok sign-in",
    optional_api_key: "Endpoint + optional key",
  })[method];

const oauthProviderFor = (method: InferenceAuthMethod): OauthProvider | null =>
  method === "chat_gpt_oauth"
    ? "openai"
    : method === "claude_oauth"
      ? "anthropic"
      : method === "grok_oauth"
        ? "grok"
        : null;

export function providerSignInState(accounts: readonly ProviderAccountView[]) {
  const next: Partial<Record<ProviderId, string>> = {};
  for (const [providerId, credentialKind] of Object.entries(PROVIDER_CREDENTIAL_KIND)) {
    const account = accounts.find(
      (entry) =>
        entry.enabled && !entry.pendingSave && entry.provider === credentialKind,
    );
    if (account) next[providerId as OauthProvider] = account.credentialId;
  }
  return next;
}

/** Providers whose completed sign-in the bridge holds after a failed save. */
export function providerPendingSaveState(accounts: readonly ProviderAccountView[]) {
  const next: Partial<Record<ProviderId, true>> = {};
  for (const [providerId, credentialKind] of Object.entries(PROVIDER_CREDENTIAL_KIND)) {
    if (
      accounts.some((entry) => entry.pendingSave && entry.provider === credentialKind)
    )
      next[providerId as OauthProvider] = true;
  }
  return next;
}

function withoutProvider<T>(state: Partial<Record<ProviderId, T>>, id: ProviderId) {
  const next = { ...state };
  delete next[id];
  return next;
}

const notAdded = (label: string) =>
  `This sign-in refreshed the account stored as ${label}. No account was added.`;

export function InferenceSetup({
  onDone,
  purpose,
  agentDid,
  onCancel,
  onBack,
  checkRuntime,
  provider: fixedProvider,
}: {
  onDone: (snapshot: DesktopClientSnapshot) => void;
  purpose: "onboarding" | "add-backend";
  agentDid?: string;
  onCancel?: () => void;
  /** back to the step before, when there is one */
  onBack?: () => void;
  /** opened without this session's provisioning, so a local agent's managed
      runtime may not be serving */
  checkRuntime: boolean;
  /* a catalog row was chosen, so the form is that provider's inputs only */
  provider?: ProviderId;
}) {
  const bootstrap = useBootstrap();
  const {
    api,
    actions: { changeConfig },
  } = useApp();
  const selectedNode = useSelectedNode();
  const allowLocal = supportsLocalManagedServer();
  const [busy, setBusy] = useState(false);
  /* which account operation holds busy, so its controls say what is running
     and Cancel only ever cancels a sign-in */
  const [accountOp, setAccountOp] = useState<"signIn" | "retrySave" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [managedWait, setManagedWait] = useState<ManagedServerWait | null>(null);
  const [catalog, setCatalog] = useState<InferenceSetupCatalog | null>(null);
  /* a failed catalog read, kept apart from the form's error; clearing it
     reads the catalog again */
  const [catalogFailure, setCatalogFailure] = useState<string | null>(null);
  const [provider, setProvider] = useState<ProviderId>(fixedProvider ?? "openai");
  const [connections, setConnections] = useState<
    Partial<Record<ProviderId, ConnectionDraft>>
  >({});
  const [signedIn, setSignedIn] = useState<Partial<Record<ProviderId, string>>>({});
  const [storedAccounts, setStoredAccounts] = useState<ProviderAccountView[]>([]);
  const [accountLabel, setAccountLabel] = useState("");
  const [signInHint, setSignInHint] = useState<string | null>(null);
  const accountRevision = useRef(0);
  const setupAgentDid = agentDid ?? selectedNode?.agentDid;
  const setupAgentDidRef = useRef(setupAgentDid);
  setupAgentDidRef.current = setupAgentDid;
  /* Setup re-entry opens at the provider step without first run's
     provisioning, so a local agent's managed runtime may not be serving.
     Provider sign-in and the final save both write through it. */
  const setupDeployment = useFleet((s) => nodeOf(s, setupAgentDid));
  const requiresManagedRuntime = Boolean(
    checkRuntime &&
    allowLocal &&
    api.managedServerStatus &&
    setupDeployment &&
    isLocalAgent(setupDeployment, bootstrap?.initAgentDid),
  );
  const [runtimeGate, setRuntimeGate] = useState<
    "idle" | "checking" | "ready" | "unavailable"
  >("idle");
  const runtimeFallbackName = bootstrap?.initAgentName?.trim() || "Local Agent";
  const checkManagedRuntime = async () => {
    setRuntimeGate("checking");
    setError(null);
    try {
      await ensureManagedRuntimeServing(api, runtimeFallbackName, {
        onWait: setManagedWait,
      });
      setRuntimeGate("ready");
      /* Account lookup goes through the runtime, so repeat it once it serves. */
      if (setupAgentDidRef.current) void observeAccounts(setupAgentDidRef.current);
    } catch (cause) {
      setRuntimeGate("unavailable");
      setError(setupErrorMessage(cause));
    }
  };
  useEffect(() => {
    if (!requiresManagedRuntime || runtimeGate !== "idle") return;
    void checkManagedRuntime();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requiresManagedRuntime, runtimeGate]);
  const [pendingSave, setPendingSave] = useState<Partial<Record<ProviderId, true>>>({});
  /* Sign-in state is known, so a chosen provider may start its sign-in. */
  const [accountsObserved, setAccountsObserved] = useState(false);
  const observeAccounts = (agentDid: string) => {
    const revision = ++accountRevision.current;
    if (!api.listProviderAccounts) {
      setAccountsObserved(true);
      return Promise.resolve();
    }
    return api
      .listProviderAccounts(agentDid)
      .then((accounts) => {
        if (
          accountRevision.current !== revision ||
          setupAgentDidRef.current !== agentDid
        )
          return;
        setStoredAccounts(accounts);
        /* adding a backend signs in a further account; only its own sign-in connects */
        if (purpose !== "add-backend") setSignedIn(providerSignInState(accounts));
        setPendingSave(providerPendingSaveState(accounts));
        setAccountsObserved(true);
      })
      .catch(() => {
        /* Sign-in remains available if account lookup fails, but only a
           click starts it: an unknown account may already be connected. */
      });
  };
  useEffect(() => {
    setSignedIn({});
    setPendingSave({});
    setAccountsObserved(false);
    if (!setupAgentDid) {
      accountRevision.current += 1;
      return;
    }
    void observeAccounts(setupAgentDid);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, setupAgentDid]);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  /* The provider the user just chose, whose sign-in starts without a click. */
  const autoSignIn = useRef<ProviderId | null>(fixedProvider ?? null);
  const [autoSignInRequest, setAutoSignInRequest] = useState(0);
  const [discovery, setDiscovery] = useState<InferenceDiscoveryResult | null>(null);
  const [modelSearch, setModelSearch] = useState("");
  const [modelPickerOpen, setModelPickerOpen] = useState(true);
  const [model, setModel] = useState("");
  const [manualModel, setManualModel] = useState(false);
  const [settings, setSettings] = useState<InferenceSettingsDraft | null>(null);
  const [selectedRecommendation, setSelectedRecommendation] =
    useState<InferenceModelRecommendation | null>(null);
  const [customize, setCustomize] = useState(false);
  const discoveryRevision = useRef(0);
  const currentDiscoveryKey = useRef("");
  const connection = connections[provider];
  const providerOption = catalog?.providers.find((option) => option.id === provider);

  useEffect(() => {
    if (catalog || catalogFailure) return;
    void api
      .getInferenceSetupCatalog()
      .then((next) => {
        setCatalog(next);
        setConnections((current) => {
          const initialized = { ...current };
          for (const option of next.providers) {
            initialized[option.id] ??= {
              authMethod: option.defaultAuthMethod,
              endpoint: option.defaultEndpoint,
              apiKey: "",
            };
          }
          return initialized;
        });
        if (next.providers[0] && !fixedProvider) setProvider(next.providers[0].id);
      })
      .catch((cause) => setCatalogFailure(setupErrorMessage(cause)));
  }, [api, catalog, catalogFailure]);

  const accountsOf = (oauthProvider: OauthProvider) =>
    storedAccounts.filter(
      (account) =>
        !account.pendingSave &&
        account.provider === PROVIDER_CREDENTIAL_KIND[oauthProvider],
    );
  const signIn = async () => {
    if (!connection) return;
    const oauthProvider = oauthProviderFor(connection.authMethod);
    if (!oauthProvider) return;
    if (requiresManagedRuntime && runtimeGate !== "ready") return;
    autoSignIn.current = null;
    const label = accountLabel.trim();
    if (label && accountsOf(oauthProvider).some((account) => account.label === label)) {
      setError(`Another account is already labelled “${label}”. Choose another label.`);
      return;
    }
    setBusy(true);
    setAccountOp("signIn");
    setError(null);
    setAuthUrl(null);
    setSignInHint(null);
    let unlisten = () => {};
    let agentDid: string | undefined;
    try {
      unlisten = await watchProviderLoginUrl(oauthProvider, setAuthUrl);
      const snapshot = await api.fetchDesktopSnapshot();
      agentDid = setupAgentDid ?? snapshot.client?.deployments[0]?.agentDid;
      if (!agentDid) throw new Error("No agent to sign in");
      const result =
        oauthProvider === "openai"
          ? await (label
              ? api.codexLogin(agentDid, null, label)
              : api.codexLogin(agentDid))
          : oauthProvider === "anthropic"
            ? await (label
                ? api.claudeLogin(agentDid, null, label)
                : api.claudeLogin(agentDid))
            : await (label
                ? api.grokLogin(agentDid, null, label)
                : api.grokLogin(agentDid));
      if (setupAgentDidRef.current !== agentDid) return;
      accountRevision.current += 1;
      setPendingSave((current) => withoutProvider(current, provider));
      setAuthUrl(null);
      const outcome = result.signIn;
      if (outcome.result === "refreshed") setSignInHint(outcome.hint);
      /* an added account's backend was created with the account, and a refreshed
         one already has its backend: nothing to save */
      if (
        purpose === "add-backend" &&
        (outcome.accountRef !== null || outcome.result === "refreshed")
      ) {
        if (outcome.result === "added") onDone(await api.fetchDesktopSnapshot());
        else setSignInHint(outcome.hint ?? notAdded(outcome.label));
        return;
      }
      setSignedIn((current) => ({ ...current, [provider]: result.credentialId }));
      invalidateDiscovery();
    } catch (cause) {
      if (agentDid && bridgeErrorCode(cause) === CREDENTIAL_NOT_SAVED) {
        setAuthUrl(null);
        void observeAccounts(agentDid);
      }
      setError(setupErrorMessage(cause));
    } finally {
      unlisten();
      setBusy(false);
      setAccountOp(null);
    }
  };

  const retrySaveSignIn = async () => {
    const agentDid = setupAgentDid;
    const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
    if (!agentDid || !oauthProvider || !api.retrySaveProviderAccount) return;
    const pendingProvider = provider;
    /* with a stored account, a store adds only under a new reference, so a
       retry returning no reference refreshed the original account */
    const hadStored = accountsOf(oauthProvider).length > 0;
    setBusy(true);
    setAccountOp("retrySave");
    setError(null);
    try {
      if (requiresManagedRuntime) {
        await ensureManagedRuntimeServing(api, runtimeFallbackName);
        setRuntimeGate("ready");
      }
      const account = await api.retrySaveProviderAccount(
        agentDid,
        PROVIDER_CREDENTIAL_KIND[oauthProvider],
      );
      if (setupAgentDidRef.current !== agentDid) return;
      accountRevision.current += 1;
      if (purpose === "add-backend" && account.accountRef !== null) {
        onDone(await api.fetchDesktopSnapshot());
        return;
      }
      setPendingSave((current) => withoutProvider(current, pendingProvider));
      if (purpose === "add-backend" && hadStored) {
        setSignInHint(notAdded(account.label));
        return;
      }
      setSignedIn((current) => ({
        ...current,
        [pendingProvider]: account.credentialId,
      }));
      invalidateDiscovery();
    } catch (cause) {
      if (bridgeErrorCode(cause) === "notFound") void observeAccounts(agentDid);
      setError(setupErrorMessage(cause));
    } finally {
      setBusy(false);
      setAccountOp(null);
    }
  };

  const cancelSignIn = () => {
    const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
    if (oauthProvider === "openai") void api.cancelCodexLogin();
    else if (oauthProvider === "anthropic") void api.cancelClaudeLogin();
    else if (oauthProvider === "grok") void api.cancelGrokLogin();
  };

  const invalidateDiscovery = () => {
    discoveryRevision.current += 1;
    currentDiscoveryKey.current = "";
    setDiscovery(null);
    setModel("");
    setManualModel(false);
    setSelectedRecommendation(null);
    setSettings(null);
    setCustomize(false);
  };

  const updateConnection = (changes: Partial<ConnectionDraft>) => {
    setConnections((current) => ({
      ...current,
      [provider]: { ...current[provider]!, ...changes },
    }));
    invalidateDiscovery();
    setError(null);
  };

  const discoverModels = async () => {
    if (!connection || busy) return;
    setBusy(true);
    setError(null);
    const requestKey = inferenceDiscoveryKey(
      ++discoveryRevision.current,
      provider,
      connection.authMethod,
      connection.endpoint,
    );
    currentDiscoveryKey.current = requestKey;
    try {
      const snapshot = await api.fetchDesktopSnapshot();
      const agentDid = setupAgentDid ?? snapshot.client?.deployments[0]?.agentDid;
      if (!agentDid) {
        setBusy(false);
        setError("No agent to configure");
        return;
      }
      const result = await api.discoverInferenceModels({
        requestKey,
        agentDid,
        provider,
        authMethod: connection.authMethod,
        endpoint: connection.endpoint,
        apiKey: connection.apiKey.trim() || null,
      });
      const current = currentInferenceDiscovery(currentDiscoveryKey.current, result);
      if (!current) return;
      setDiscovery(current);
      // Discovery supplies choices, but the user makes the one model decision.
      // Even a one-item catalog is never accepted implicitly.
      setModel("");
      setSelectedRecommendation(null);
      setSettings(null);
    } catch (cause) {
      if (currentDiscoveryKey.current !== requestKey) return;
      setError(setupErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const chooseModel = (option: InferenceModelOption) => {
    setModel(option.advertised.model_name);
    setSelectedRecommendation(option.recommendation);
    setSettings(recommendedInferenceSettings(option.recommendation));
    setCustomize(false);
    setModelPickerOpen(false);
  };

  const describeManualModel = async () => {
    if (!connection || !model.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const recommendation = await api.getInferenceModelRecommendation({
        provider,
        authMethod: connection.authMethod,
        modelName: model.trim(),
        displayName: null,
        contextWindow: null,
        maxContextWindow: null,
        maxOutputTokens: null,
        reasoningEfforts: null,
      });
      setSelectedRecommendation(recommendation);
      setSettings(recommendedInferenceSettings(recommendation));
    } catch (cause) {
      setError(setupErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const persistInference = async () => {
    if (!connection || !discovery || !selectedRecommendation || !settings)
      throw new Error("Complete provider discovery and model selection first");
    const settingsError = validateInferenceSettings(selectedRecommendation, settings);
    if (settingsError) throw new Error(settingsError);
    const snapshot = await api.fetchDesktopSnapshot();
    const deployment = snapshot.client?.deployments.find(
      (candidate) => candidate.agentDid === setupAgentDid,
    );
    if (!deployment) throw new Error("No agent to configure");

    const plan = buildInferenceSetupPlan({
      purpose,
      deployment,
      provider,
      apiKey: connection.apiKey,
      oauth: oauthProviderFor(connection.authMethod) !== null,
      discovery,
      model,
      recommendation: selectedRecommendation,
      settings,
    });
    await changeConfig("applyConfigComponents", { document: plan.document });
    // Discovery ran before the backend existed. Running it again publishes the
    // advertised catalog onto the persisted backend. It stays off the save path;
    // the runtime prober publishes subscription catalogs if it fails.
    const publishKey = `${currentDiscoveryKey.current}:publish`;
    void Promise.resolve()
      .then(() =>
        api.discoverInferenceModels({
          requestKey: publishKey,
          agentDid: deployment.agentDid,
          provider,
          authMethod: connection.authMethod,
          endpoint: connection.endpoint,
          apiKey: connection.apiKey.trim() || null,
        }),
      )
      .catch(() => {});
    return plan;
  };

  const waitForSelectedBehavior = async (
    profileId: string,
    defaultBehaviorId: string | null,
  ) => {
    const deadline = Date.now() + SAVE_CONFIRMATION_TIMEOUT_MS;
    while (Date.now() < deadline) {
      const snapshot = await api.fetchDesktopSnapshot();
      const deployment = snapshot.client?.deployments.find(
        (candidate) => candidate.agentDid === setupAgentDid,
      );
      const bound = deployment?.behaviorConfigs.find(
        (behavior) =>
          behavior.behavior_id === defaultBehaviorId &&
          behavior.inference_profile_id === profileId,
      );
      const ready = deployment?.behaviorReadiness.behaviors.some(
        (status) => status.state === "ready" && status.behaviorId === defaultBehaviorId,
      );
      const savedProfile = deployment?.inferenceProfiles.find(
        (profile) => profile.profile_id === profileId,
      );
      const savedBackend = deployment?.inferenceBackends.find(
        (backend) => backend.backendId === savedProfile?.backend_id,
      );
      const savedSelection =
        savedProfile?.model_name === model.trim() &&
        (savedProfile?.reasoning_effort ?? null) ===
          (settings?.reasoningEffort || null) &&
        savedBackend?.providerKind === discovery?.providerKind &&
        savedBackend?.endpoint === discovery?.effectiveEndpoint;
      if (savedSelection && (purpose === "add-backend" || (bound && ready)))
        return snapshot;
      await new Promise((resolve) => window.setTimeout(resolve, 250));
    }
    throw new Error(
      "The runtime has not confirmed the saved inference configuration. Your selection is retained; retry saving.",
    );
  };

  const saveInference = async () => {
    setBusy(true);
    setError(null);
    try {
      const { profileId, defaultBehaviorId } = await persistInference();
      const snapshot = await waitForSelectedBehavior(profileId, defaultBehaviorId);
      onDone(snapshot);
    } catch (cause) {
      setError(setupErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const pickProvider = (id: ProviderId) => {
    if (busy) return;
    autoSignIn.current = id;
    if (id === provider) {
      /* Choosing the preselected provider is a choice too. */
      setAutoSignInRequest((count) => count + 1);
      return;
    }
    setProvider(id);
    setAuthUrl(null);
    setError(null);
    invalidateDiscovery();
  };
  useEffect(() => {
    if (
      autoSignIn.current !== provider ||
      !catalog ||
      !connection ||
      !oauthProviderFor(connection.authMethod) ||
      !accountsObserved ||
      busy ||
      (requiresManagedRuntime && runtimeGate !== "ready")
    )
      return;
    autoSignIn.current = null;
    if (signedIn[provider] || pendingSave[provider]) return;
    const oauthProvider = oauthProviderFor(connection.authMethod)!;
    if (purpose === "add-backend" && accountsOf(oauthProvider).length) return;
    void signIn();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    autoSignInRequest,
    provider,
    catalog,
    connection,
    accountsObserved,
    busy,
    requiresManagedRuntime,
    runtimeGate,
    signedIn,
    pendingSave,
    storedAccounts,
  ]);
  if (
    requiresManagedRuntime &&
    runtimeGate !== "ready" &&
    Object.keys(pendingSave).length === 0
  ) {
    return (
      <Frame embedded={purpose === "add-backend"}>
        <Title note="Provider sign-in and the saved configuration are stored by your local agent.">
          {purpose === "add-backend"
            ? "Add an inference backend"
            : "Choose an inference provider"}
        </Title>
        {runtimeGate === "unavailable" ? (
          <div className="grid gap-3">
            <p role="alert" className="text-sm text-destructive">
              {error ??
                "The local agent is not running, so provider sign-in is unavailable."}
            </p>
            <Button
              variant="brand"
              className="justify-self-start"
              onClick={() => void checkManagedRuntime()}
            >
              Try again
            </Button>
          </div>
        ) : (
          <div className="grid gap-3">
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Starting your local agent…
            </p>
            {managedWait ? (
              <ManagedServerWaitNotice
                wait={managedWait}
                onOpenLoginItems={api.openManagedServerLoginItems}
              />
            ) : null}
          </div>
        )}
        <Nav onBack={onCancel} />
      </Frame>
    );
  }
  const advertised = discovery?.models.find(
    (option) => option.advertised.model_name === model,
  )?.advertised;
  const filteredModels =
    discovery?.models.filter((option) => {
      const query = modelSearch.trim().toLocaleLowerCase();
      return (
        !query ||
        option.advertised.model_name.toLocaleLowerCase().includes(query) ||
        option.advertised.display_name?.toLocaleLowerCase().includes(query)
      );
    }) ?? [];
  const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
  const connectionReady = Boolean(
    connection?.endpoint.trim() &&
    (oauthProvider
      ? signedIn[provider]
      : connection.authMethod === "optional_api_key" || connection.apiKey.trim()),
  );

  const authOptions = providerOption?.authOptions ?? [];
  const providerDetails = (
    <div className="grid gap-4">
      <div className="grid gap-3 text-sm">
        {connection && authOptions.length > 1 ? (
          <Field label="Connection method">
            <Select
              items={authOptions.map((option) => ({
                value: option.method,
                label: option.displayName,
              }))}
              disabled={busy}
              value={connection.authMethod}
              onValueChange={(next) => {
                if (!next) return;
                const option = authOptions.find((item) => item.method === next);
                autoSignIn.current = provider;
                updateConnection({
                  authMethod: next as InferenceAuthMethod,
                  endpoint: option?.defaultEndpoint ?? connection.endpoint,
                  apiKey: "",
                });
              }}
            >
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {authOptions.map((option) => (
                  <SelectItem key={option.method} value={option.method}>
                    {option.displayName}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
        ) : null}
        {oauthProvider ? (
          <div className="grid gap-3">
            {signedIn[provider] ? (
              <p className="flex items-center gap-2">
                <CircleCheck className="size-4" />
                Account connected
              </p>
            ) : (
              <>
                {purpose === "add-backend" ? (
                  <Field label="Account label">
                    <Input
                      disabled={busy}
                      value={accountLabel}
                      onChange={(event) => setAccountLabel(event.target.value)}
                      placeholder="Optional, e.g. Work"
                    />
                  </Field>
                ) : null}
                <div className="flex items-center justify-between gap-3">
                  <p className="text-muted-foreground">
                    {authLabel(connection!.authMethod)}
                  </p>
                  <span className="flex gap-2">
                    {accountOp === "signIn" && (
                      <Button variant="outline" onClick={cancelSignIn}>
                        Cancel
                      </Button>
                    )}
                    {accountOp !== "signIn" &&
                      pendingSave[provider] &&
                      api.retrySaveProviderAccount && (
                        <Button
                          variant="brand"
                          disabled={busy}
                          onClick={retrySaveSignIn}
                        >
                          {accountOp === "retrySave" && <Spinner />}
                          {accountOp === "retrySave" ? "Saving…" : "Retry save"}
                        </Button>
                      )}
                    <Button
                      variant={pendingSave[provider] && !busy ? "outline" : "brand"}
                      disabled={busy}
                      onClick={signIn}
                    >
                      {accountOp === "signIn" && <Spinner />}
                      {accountOp === "signIn" ? "Waiting…" : "Sign in"}
                    </Button>
                  </span>
                </div>
              </>
            )}
            {signInHint ? <p className="text-muted-foreground">{signInHint}</p> : null}
            {authUrl ? (
              <button
                type="button"
                className="justify-self-start text-xs underline"
                onClick={() => void openExternalUrl(authUrl)}
              >
                Open the sign-in page
              </button>
            ) : null}
          </div>
        ) : (
          <>
            <Field
              label={
                connection?.authMethod === "optional_api_key"
                  ? "API key (optional)"
                  : "API key"
              }
            >
              <Input
                type="password"
                disabled={busy}
                value={connection?.apiKey ?? ""}
                onChange={(event) => updateConnection({ apiKey: event.target.value })}
                placeholder="Stored only when you save"
              />
            </Field>
            <Field label="Endpoint">
              <Input
                disabled={busy}
                value={connection?.endpoint ?? ""}
                className="font-mono"
                onChange={(event) => updateConnection({ endpoint: event.target.value })}
              />
            </Field>
          </>
        )}
      </div>
      <Button
        variant="outline"
        disabled={busy || !connectionReady}
        onClick={discoverModels}
      >
        {busy ? <Spinner /> : null}{" "}
        {discovery
          ? "Refresh models"
          : oauthProvider && signedIn[provider]
            ? "Find models"
            : "Connect and find models"}
      </Button>
      {discovery ? (
        <section
          className="grid gap-3 border-t border-border/60 pt-4"
          aria-label="Model selection"
        >
          <h2 className="text-sm font-medium">Choose a model</h2>
          {discovery?.failure ? (
            <div className="mb-4 rounded-xl border border-destructive/30 p-3">
              <p className="text-sm text-destructive">{discovery.failure.message}</p>
              <Button
                className="mt-3"
                variant="outline"
                disabled={busy}
                onClick={discoverModels}
              >
                Retry connection
              </Button>
            </div>
          ) : null}
          {discovery?.models.length ? (
            model && !modelPickerOpen ? (
              <div className="flex items-center justify-between gap-3 rounded-xl border border-border/60 p-3">
                <span className="min-w-0 break-words text-sm font-medium">{model}</span>
                <Button
                  variant="outline"
                  disabled={busy}
                  onClick={() => setModelPickerOpen(true)}
                >
                  Change model
                </Button>
              </div>
            ) : (
              <div className="grid gap-3">
                <Input
                  disabled={busy}
                  value={modelSearch}
                  onChange={(event) => setModelSearch(event.target.value)}
                  placeholder="Search advertised models"
                  aria-label="Search advertised models"
                />
                <div
                  role="listbox"
                  aria-label="Advertised models"
                  className="max-h-40 overflow-y-auto rounded-xl border border-border/60 p-1"
                >
                  {filteredModels.map((option) => (
                    <button
                      key={option.advertised.model_name}
                      type="button"
                      role="option"
                      disabled={busy}
                      aria-selected={model === option.advertised.model_name}
                      className={cn(
                        "block w-full rounded-lg px-3 py-2 text-left text-sm",
                        model === option.advertised.model_name
                          ? "bg-accent text-foreground"
                          : "hover:bg-accent/60",
                      )}
                      onClick={() => chooseModel(option)}
                    >
                      <span className="block font-medium">
                        {option.advertised.display_name ?? option.advertised.model_name}
                      </span>
                      {option.advertised.display_name ? (
                        <span className="block font-mono text-xs text-muted-foreground">
                          {option.advertised.model_name}
                        </span>
                      ) : null}
                    </button>
                  ))}
                </div>
              </div>
            )
          ) : discovery?.manualEntryAllowed ? (
            <div className="grid gap-3">
              <p className="text-sm text-muted-foreground">
                Model discovery is unavailable. Manual entry is enabled as an explicit
                fallback and will be saved exactly as entered.
              </p>
              <Field label="Manual model identifier">
                <Input
                  disabled={busy}
                  value={model}
                  onChange={(event) => {
                    setModel(event.target.value);
                    setManualModel(true);
                    setSelectedRecommendation(null);
                    setSettings(null);
                  }}
                  placeholder="Exact served model ID"
                />
              </Field>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">No discovery result.</p>
          )}
          {manualModel && !selectedRecommendation ? (
            <Button
              variant="outline"
              disabled={busy || !model.trim()}
              onClick={describeManualModel}
            >
              {busy ? <Spinner /> : null} Load model defaults
            </Button>
          ) : null}
        </section>
      ) : null}
      {selectedRecommendation && settings ? (
        <section
          className="grid gap-3 border-t border-border/60 pt-4"
          aria-label="Model defaults"
        >
          <h2 className="text-sm font-medium">Model defaults</h2>
          <fieldset disabled={busy} className="min-w-0">
            <div className="mb-4 rounded-2xl border border-border/60 bg-raised p-4 text-sm">
              <p className="font-medium">{model}</p>
              <p className="mt-2 text-xs text-muted-foreground">
                Model defaults and limits
              </p>
              <dl className="mt-1 grid grid-cols-2 gap-2 text-xs">
                <div>
                  <dt className="text-muted-foreground">Default context</dt>
                  <dd>
                    {(
                      advertised?.context_window ??
                      selectedRecommendation.contextWindow?.recommended
                    )?.toLocaleString() ?? "Not advertised"}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Max output</dt>
                  <dd>
                    {discovery?.providerKind === "ChatGptCodex"
                      ? "Provider managed"
                      : ((
                          advertised?.max_output_tokens ??
                          selectedRecommendation.maxOutputTokens?.max
                        )?.toLocaleString() ?? "Not advertised")}
                  </dd>
                </div>
              </dl>
            </div>
            {selectedRecommendation && settings ? (
              <InferenceModelControls
                recommendation={selectedRecommendation}
                value={settings}
                onChange={setSettings}
                expanded={customize}
                onExpandedChange={setCustomize}
              />
            ) : null}
          </fieldset>
          <Button
            data-testid="setup-save-inference"
            variant="brand"
            disabled={busy}
            onClick={saveInference}
          >
            {busy ? <Spinner /> : null}{" "}
            {purpose === "add-backend" ? "Save backend" : "Save and start chatting"}{" "}
            <ArrowRight />
          </Button>
        </section>
      ) : null}
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );

  return (
    <Frame
      embedded={purpose === "add-backend"}
      onBack={purpose === "add-backend" ? onCancel : undefined}
    >
      {fixedProvider ? (
        <header className="mb-6">
          <h2 className="font-heading text-lg text-heading">
            Set up{" "}
            {catalog?.providers.find((o) => o.id === fixedProvider)?.displayName ??
              fixedProvider}
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {catalog?.providers.find((o) => o.id === fixedProvider)?.description}
          </p>
        </header>
      ) : (
        <Title note="Choose a provider, connect, then select a model and its defaults—all here.">
          {purpose === "add-backend"
            ? "Add an inference backend"
            : "Choose an inference provider"}
        </Title>
      )}
      {catalog && fixedProvider ? (
        /* the chosen provider's own inputs, no grid */
        <div className="rounded-3xl bg-raised px-5 py-4 shadow-sm ring-1 ring-foreground/5">
          {providerDetails}
        </div>
      ) : catalog ? (
        <div className="grid gap-3" role="radiogroup" aria-label="Inference provider">
          {catalog.providers.map((option) => {
            const visual = PROVIDER_VISUALS[option.id];
            return (
              <Option
                key={option.id}
                selected={provider === option.id}
                disabled={busy}
                onSelect={() => pickProvider(option.id)}
                title={option.displayName}
                hint={option.description}
                icon={visual.icon}
                logo={visual.logo}
                testId={`setup-provider-${option.id}`}
              >
                {provider === option.id ? providerDetails : null}
              </Option>
            );
          })}
        </div>
      ) : (
        <div className="grid gap-3">
          {catalogFailure ? (
            <>
              <p role="alert" className="text-sm text-destructive">
                {catalogFailure}
              </p>
              <Button
                variant="outline"
                className="justify-self-start"
                onClick={() => setCatalogFailure(null)}
              >
                Try again
              </Button>
            </>
          ) : (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Loading provider options…
            </p>
          )}
        </div>
      )}
      {!fixedProvider && <Nav onBack={busy ? undefined : (onCancel ?? onBack)} />}
    </Frame>
  );
}
