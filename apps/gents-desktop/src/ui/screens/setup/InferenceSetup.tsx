/* The inference step: choose a provider, connect or sign in, pick a model
   and its defaults, and save them in one operator transaction. Shared by
   first run, setup re-entry and adding a backend from the agent screen.
   Provider/model guidance comes from the versioned Rust contract. */
import { useEffect, useMemo, useReducer, useRef, useState } from "react";
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
  InferenceModelOption,
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
  validateInferenceSettings,
} from "../inference/InferenceModelControls";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useSelectedNode } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf } from "../../../hooks/fleetStore";
import { Field, Frame, Nav, Option, Title } from "./parts";
import { useSetupCatalog } from "@/hooks/useProviders";
import {
  connectionDefaults,
  initialSetupForm,
  setupFormReducer,
  type ConnectionDraft,
  type ProviderId,
} from "./inferenceSetupForm";

/* The runtime confirms a save only after it reconciles the new documents,
   starts the rebound behavior and replicates its readiness back. */
const SAVE_CONFIRMATION_TIMEOUT_MS = 15_000;

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
  const [form, dispatch] = useReducer(
    setupFormReducer,
    fixedProvider,
    initialSetupForm,
  );
  const [managedWait, setManagedWait] = useState<ManagedServerWait | null>(null);
  const { catalog, error: catalogFailure, retry: retryCatalog } = useSetupCatalog();
  const busy = form.op !== null;
  const { error, runtimeGate, accountLabel, signInHint, authUrl } = form;
  const { signedIn, pendingSave, stored: storedAccounts } = form.accounts;
  const {
    discovery,
    search: modelSearch,
    pickerOpen: modelPickerOpen,
    name: model,
    manual: manualModel,
    recommendation: selectedRecommendation,
    settings,
    customize,
  } = form.model;
  /* which account operation holds the form, so its controls say what is
     running and Cancel only ever cancels a sign-in */
  const accountOp = form.op === "signIn" || form.op === "retrySave" ? form.op : null;
  const provider: ProviderId = form.provider ?? catalog?.providers[0]?.id ?? "openai";
  const providerOption = catalog?.providers.find((option) => option.id === provider);
  const edited = form.connections[provider];
  const connection = useMemo(
    () => edited ?? (providerOption ? connectionDefaults(providerOption) : undefined),
    [edited, providerOption],
  );
  /* reads that answer for another agent, or before a sign-in that changed
     what they would say, are dropped */
  const accountRevision = useRef(0);
  const setupAgentDid = agentDid ?? selectedNode?.agentDid;
  const setupAgentDidRef = useRef(setupAgentDid);
  setupAgentDidRef.current = setupAgentDid;
  const discoveryRevision = useRef(0);
  const currentDiscoveryKey = useRef("");
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
  const runtimeFallbackName = bootstrap?.initAgentName?.trim() || "Local Agent";
  const checkManagedRuntime = async () => {
    dispatch({ type: "runtimeGate", gate: "checking" });
    try {
      await ensureManagedRuntimeServing(api, runtimeFallbackName, {
        onWait: setManagedWait,
      });
      dispatch({ type: "runtimeGate", gate: "ready" });
      /* Account lookup goes through the runtime, so repeat it once it serves. */
      if (setupAgentDidRef.current) void observeAccounts(setupAgentDidRef.current);
    } catch (cause) {
      dispatch({
        type: "runtimeGate",
        gate: "unavailable",
        error: setupErrorMessage(cause),
      });
    }
  };
  useEffect(() => {
    if (!requiresManagedRuntime || runtimeGate !== "idle") return;
    void checkManagedRuntime();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requiresManagedRuntime, runtimeGate]);
  const observeAccounts = (agentDid: string) => {
    const revision = ++accountRevision.current;
    const seedSignedIn = purpose !== "add-backend";
    if (!api.listProviderAccounts) {
      dispatch({ type: "accountsRead", accounts: [], seedSignedIn: false });
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
        dispatch({ type: "accountsRead", accounts, seedSignedIn });
      })
      .catch(() => {
        /* Sign-in remains available if account lookup fails, but only a
           click starts it: an unknown account may already be connected. */
      });
  };
  useEffect(() => {
    dispatch({ type: "accountsCleared" });
    if (!setupAgentDid) {
      accountRevision.current += 1;
      return;
    }
    void observeAccounts(setupAgentDid);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, setupAgentDid]);

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
    dispatch({ type: "autoSignInTaken" });
    const label = accountLabel.trim();
    if (label && accountsOf(oauthProvider).some((account) => account.label === label)) {
      dispatch({
        type: "failed",
        error: `Another account is already labelled “${label}”. Choose another label.`,
      });
      return;
    }
    dispatch({ type: "opStarted", op: "signIn" });
    let unlisten = () => {};
    let agentDid: string | undefined;
    try {
      unlisten = await watchProviderLoginUrl(oauthProvider, (url) =>
        dispatch({ type: "authUrl", url }),
      );
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
      dispatch({ type: "pendingSaveCleared", provider });
      dispatch({ type: "authUrl", url: null });
      const outcome = result.signIn;
      if (outcome.result === "refreshed")
        dispatch({ type: "hint", hint: outcome.hint });
      /* an added account's backend was created with the account, and a refreshed
         one already has its backend: nothing to save */
      if (
        purpose === "add-backend" &&
        (outcome.accountRef !== null || outcome.result === "refreshed")
      ) {
        if (outcome.result === "added") onDone(await api.fetchDesktopSnapshot());
        else dispatch({ type: "hint", hint: outcome.hint ?? notAdded(outcome.label) });
        return;
      }
      dispatch({ type: "connected", provider, credentialId: result.credentialId });
      invalidateDiscovery();
    } catch (cause) {
      if (agentDid && bridgeErrorCode(cause) === CREDENTIAL_NOT_SAVED) {
        dispatch({ type: "authUrl", url: null });
        void observeAccounts(agentDid);
      }
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      unlisten();
      dispatch({ type: "opEnded" });
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
    dispatch({ type: "opStarted", op: "retrySave" });
    try {
      if (requiresManagedRuntime) {
        await ensureManagedRuntimeServing(api, runtimeFallbackName);
        dispatch({ type: "runtimeGate", gate: "ready" });
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
      dispatch({ type: "pendingSaveCleared", provider: pendingProvider });
      if (purpose === "add-backend" && hadStored) {
        dispatch({ type: "hint", hint: notAdded(account.label) });
        return;
      }
      dispatch({
        type: "connected",
        provider: pendingProvider,
        credentialId: account.credentialId,
      });
      invalidateDiscovery();
    } catch (cause) {
      if (bridgeErrorCode(cause) === "notFound") void observeAccounts(agentDid);
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      dispatch({ type: "opEnded" });
    }
  };

  const cancelSignIn = () => {
    const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
    if (oauthProvider === "openai") void api.cancelCodexLogin();
    else if (oauthProvider === "anthropic") void api.cancelClaudeLogin();
    else if (oauthProvider === "grok") void api.cancelGrokLogin();
  };

  /* a discovery still out answers for a connection that no longer stands */
  const invalidateDiscovery = () => {
    discoveryRevision.current += 1;
    currentDiscoveryKey.current = "";
  };

  const updateConnection = (
    changes: Partial<ConnectionDraft>,
    { autoSignIn = false } = {},
  ) => {
    if (!connection) return;
    dispatch({
      type: "connectionEdited",
      provider,
      connection: { ...connection, ...changes },
      autoSignIn,
    });
    invalidateDiscovery();
  };

  const discoverModels = async () => {
    if (!connection || busy) return;
    dispatch({ type: "opStarted", op: "discover" });
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
        dispatch({ type: "failed", error: "No agent to configure" });
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
      dispatch({ type: "modelsDiscovered", discovery: current });
    } catch (cause) {
      if (currentDiscoveryKey.current !== requestKey) return;
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      dispatch({ type: "opEnded" });
    }
  };

  const chooseModel = (option: InferenceModelOption) =>
    dispatch({ type: "modelChosen", option });

  const describeManualModel = async () => {
    if (!connection || !model.trim()) return;
    dispatch({ type: "opStarted", op: "describe" });
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
      dispatch({ type: "recommendationLoaded", recommendation });
    } catch (cause) {
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      dispatch({ type: "opEnded" });
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
    dispatch({ type: "opStarted", op: "save" });
    try {
      const { profileId, defaultBehaviorId } = await persistInference();
      const snapshot = await waitForSelectedBehavior(profileId, defaultBehaviorId);
      onDone(snapshot);
    } catch (cause) {
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      dispatch({ type: "opEnded" });
    }
  };

  const pickProvider = (id: ProviderId) => {
    if (busy) return;
    if (id !== provider) invalidateDiscovery();
    dispatch({ type: "providerPicked", provider: id, current: provider });
  };
  /* a provider just chosen starts its sign-in once the catalog, the account
     read and the runtime allow it, unless an account is already connected */
  useEffect(() => {
    if (
      form.autoSignIn?.provider !== provider ||
      !catalog ||
      !connection ||
      !oauthProviderFor(connection.authMethod) ||
      !form.accounts.observed ||
      busy ||
      (requiresManagedRuntime && runtimeGate !== "ready")
    )
      return;
    dispatch({ type: "autoSignInTaken" });
    if (signedIn[provider] || pendingSave[provider]) return;
    const oauthProvider = oauthProviderFor(connection.authMethod)!;
    if (purpose === "add-backend" && accountsOf(oauthProvider).length) return;
    void signIn();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    form.autoSignIn,
    provider,
    catalog,
    connection,
    form.accounts,
    busy,
    requiresManagedRuntime,
    runtimeGate,
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
                updateConnection(
                  {
                    authMethod: next as InferenceAuthMethod,
                    endpoint: option?.defaultEndpoint ?? connection.endpoint,
                    apiKey: "",
                  },
                  { autoSignIn: true },
                );
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
                      onChange={(event) =>
                        dispatch({ type: "labelEdited", label: event.target.value })
                      }
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
                  onClick={() => dispatch({ type: "pickerOpened" })}
                >
                  Change model
                </Button>
              </div>
            ) : (
              <div className="grid gap-3">
                <Input
                  disabled={busy}
                  value={modelSearch}
                  onChange={(event) =>
                    dispatch({ type: "searchEdited", search: event.target.value })
                  }
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
                  onChange={(event) =>
                    dispatch({ type: "manualModelTyped", name: event.target.value })
                  }
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
                onChange={(next) =>
                  dispatch({ type: "settingsEdited", settings: next })
                }
                expanded={customize}
                onExpandedChange={(expanded) =>
                  dispatch({ type: "customizeToggled", expanded })
                }
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
                onClick={() => void retryCatalog()}
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
