import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { KeyRound, Orbit, Server, Sparkles } from "lucide-react";
import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";
import { DEFAULT_NODE_NAME } from "@source-inc/gents-desktop-fleet";
import { newestWins } from "../../../lib/reads";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { ManagedServerWaitNotice } from "./StartupProgress";
import { supportsLocalManagedServer } from "../../../lib/shellPlatform";
import {
  CREDENTIAL_NOT_SAVED,
  PROVIDER_CREDENTIAL_KIND,
  type OauthProvider,
} from "@/lib/providerLogin";
import { bridgeErrorCode, setupErrorMessage } from "../../../lib/setupErrors";
import { isLocalNode } from "@/lib/firstRun";
import {
  currentInferenceDiscovery,
  inferenceDiscoveryKey,
} from "@/lib/providerDiscovery";
import { buildInferenceSetupPlan } from "@/lib/inferenceSetupPersistence";
import { validateInferenceSettings } from "../inference/InferenceModelControls";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useSelectedNode } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf } from "../../../hooks/fleetStore";
import { Frame, Nav, Option, Title } from "./parts";
import { ConnectionFields } from "./ConnectionFields";
import { ModelChoice } from "./ModelChoice";
import { useSetupCatalog } from "@/hooks/useProviders";
import {
  connectionDefaults,
  initialSetupForm,
  oauthProviderFor,
  setupFormReducer,
  type ConnectionDraft,
  type ProviderId,
} from "./inferenceSetupForm";

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

const PROVIDER_IDS: ReadonlySet<string> = new Set(Object.keys(PROVIDER_VISUALS));
export const isProviderId = (value: string): value is ProviderId =>
  PROVIDER_IDS.has(value);

const notAdded = (label: string) =>
  `This sign-in refreshed the account stored as ${label}. No account was added.`;

export function InferenceSetup({
  onDone,
  purpose,
  nodeDid,
  onCancel,
  onBack,
  checkRuntime,
  provider: fixedProvider,
}: {
  onDone: (snapshot: DesktopClientSnapshot) => void;
  purpose: "onboarding" | "add-backend";
  nodeDid?: string;
  onCancel?: () => void;
  onBack?: () => void;
  checkRuntime: boolean;
  provider?: ProviderId;
}) {
  const bootstrap = useBootstrap();
  const { stores, actions } = useApp();
  const { changeConfig } = actions;
  const selectedNode = useSelectedNode();
  const allowLocal = supportsLocalManagedServer();
  const [form, dispatch] = useReducer(
    setupFormReducer,
    fixedProvider,
    initialSetupForm,
  );
  const managedWait = stores.localServer.use.wait();
  const { catalog, error: catalogFailure, retry: retryCatalog } = useSetupCatalog();
  const busy = form.op !== null;
  const { error, runtimeGate, accountLabel } = form;
  const { signedIn, pendingSave, stored: storedAccounts } = form.accounts;
  const {
    discovery,
    name: model,
    recommendation: selectedRecommendation,
    settings,
  } = form.model;
  const provider: ProviderId = form.provider ?? catalog?.providers[0]?.id ?? "openai";
  const providerOption = catalog?.providers.find((option) => option.id === provider);
  const edited = form.connections[provider];
  const connection = useMemo(
    () => edited ?? (providerOption ? connectionDefaults(providerOption) : undefined),
    [edited, providerOption],
  );
  const oauthAccountRef =
    connection && oauthProviderFor(connection.authMethod)
      ? (signedIn[provider]?.accountRef ?? null)
      : null;
  const [accountReads] = useState(newestWins);
  const setupNodeDid = nodeDid ?? selectedNode?.nodeDid;
  const setupNodeDidRef = useRef(setupNodeDid);
  setupNodeDidRef.current = setupNodeDid;
  const discoveryRevision = useRef(0);
  const currentDiscoveryKey = useRef("");
  const setupDeployment = useFleet((s) => nodeOf(s, setupNodeDid));
  const requiresManagedRuntime = Boolean(
    checkRuntime &&
    allowLocal &&
    actions.localServerOffers.status &&
    setupDeployment &&
    isLocalNode(setupDeployment, bootstrap?.initNodeDid),
  );
  const runtimeFallbackName = bootstrap?.initNodeName?.trim() || DEFAULT_NODE_NAME;
  const checkManagedRuntime = async () => {
    dispatch({ type: "runtimeGate", gate: "checking" });
    try {
      await actions.ensureLocalServerServing(runtimeFallbackName);
      dispatch({ type: "runtimeGate", gate: "ready" });
      if (setupNodeDidRef.current) void observeAccounts(setupNodeDidRef.current);
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
  const observeAccounts = (nodeDid: string) => {
    const current = accountReads.begin();
    const seedSignedIn = purpose !== "add-backend";
    return actions.loadProviderAccounts(nodeDid).then((accounts) => {
      if (!accounts) return;
      if (!current() || setupNodeDidRef.current !== nodeDid) return;
      dispatch({ type: "accountsRead", accounts, seedSignedIn });
    });
  };
  useEffect(() => {
    dispatch({ type: "accountsCleared" });
    if (!setupNodeDid) {
      accountReads.supersede();
      return;
    }
    void observeAccounts(setupNodeDid);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setupNodeDid]);

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
    let nodeDid: string | undefined;
    try {
      const snapshot = await actions.readSnapshot();
      nodeDid = setupNodeDid ?? snapshot.client?.deployments[0]?.nodeDid;
      if (!nodeDid) throw new Error("No node to sign in");
      const result = await actions.signInToProvider(nodeDid, oauthProvider, {
        label: label || null,
        onUrl: (url) => dispatch({ type: "authUrl", url }),
      });
      if (setupNodeDidRef.current !== nodeDid) return;
      accountReads.supersede();
      dispatch({ type: "pendingSaveCleared", provider });
      dispatch({ type: "authUrl", url: null });
      const outcome = result.signIn;
      if (outcome.result === "refreshed")
        dispatch({ type: "hint", hint: outcome.hint });
      if (
        purpose === "add-backend" &&
        (outcome.accountRef !== null || outcome.result === "refreshed")
      ) {
        if (outcome.result === "added") onDone(await actions.readSnapshot());
        else dispatch({ type: "hint", hint: outcome.hint ?? notAdded(outcome.label) });
        return;
      }
      dispatch({
        type: "connected",
        provider,
        account: { credentialId: result.credentialId, accountRef: outcome.accountRef },
      });
      invalidateDiscovery();
    } catch (cause) {
      if (nodeDid && bridgeErrorCode(cause) === CREDENTIAL_NOT_SAVED) {
        dispatch({ type: "authUrl", url: null });
        void observeAccounts(nodeDid);
      }
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      dispatch({ type: "opEnded" });
    }
  };

  const retrySaveSignIn = async () => {
    const nodeDid = setupNodeDid;
    const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
    if (!nodeDid || !oauthProvider || !actions.canRetryProviderSave) return;
    const pendingProvider = provider;
    const hadStored = accountsOf(oauthProvider).length > 0;
    dispatch({ type: "opStarted", op: "retrySave" });
    try {
      if (requiresManagedRuntime) {
        await actions.ensureLocalServerServing(runtimeFallbackName);
        dispatch({ type: "runtimeGate", gate: "ready" });
      }
      const account = await actions.retrySaveProviderAccount(
        nodeDid,
        PROVIDER_CREDENTIAL_KIND[oauthProvider],
      );
      if (setupNodeDidRef.current !== nodeDid) return;
      accountReads.supersede();
      if (purpose === "add-backend" && account.accountRef !== null) {
        onDone(await actions.readSnapshot());
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
        account: { credentialId: account.credentialId, accountRef: account.accountRef },
      });
      invalidateDiscovery();
    } catch (cause) {
      if (bridgeErrorCode(cause) === "notFound") void observeAccounts(nodeDid);
      dispatch({ type: "failed", error: setupErrorMessage(cause) });
    } finally {
      dispatch({ type: "opEnded" });
    }
  };

  const useAnotherAccount = () => {
    dispatch({ type: "signedOut", provider });
    invalidateDiscovery();
  };

  const cancelSignIn = () => {
    const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
    if (oauthProvider) void actions.cancelProviderSignIn(oauthProvider);
  };

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
      const snapshot = await actions.readSnapshot();
      const nodeDid = setupNodeDid ?? snapshot.client?.deployments[0]?.nodeDid;
      if (!nodeDid) {
        dispatch({ type: "failed", error: "No node to configure" });
        return;
      }
      const result = await actions.discoverInferenceModels({
        requestKey,
        nodeDid,
        provider,
        authMethod: connection.authMethod,
        endpoint: connection.endpoint,
        apiKey: connection.apiKey.trim() || null,
        accountRef: oauthAccountRef,
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

  const describeManualModel = async () => {
    if (!connection || !model.trim()) return;
    dispatch({ type: "opStarted", op: "describe" });
    try {
      const recommendation = await actions.getInferenceModelRecommendation({
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
    const snapshot = await actions.readSnapshot();
    const deployment = snapshot.client?.deployments.find(
      (candidate) => candidate.nodeDid === setupNodeDid,
    );
    if (!deployment) throw new Error("No node to configure");

    const plan = buildInferenceSetupPlan({
      purpose,
      deployment,
      provider,
      apiKey: connection.apiKey,
      oauth: oauthProviderFor(connection.authMethod) !== null,
      accountRef: oauthAccountRef,
      discovery,
      model,
      recommendation: selectedRecommendation,
      settings,
    });
    await changeConfig("applyConfigComponents", { document: plan.document });
    const publishKey = `${currentDiscoveryKey.current}:publish`;
    void Promise.resolve()
      .then(() =>
        actions.discoverInferenceModels({
          requestKey: publishKey,
          nodeDid: deployment.nodeDid,
          provider,
          authMethod: connection.authMethod,
          endpoint: connection.endpoint,
          apiKey: connection.apiKey.trim() || null,
          accountRef: oauthAccountRef,
        }),
      )
      .catch(() => {});
    return plan;
  };

  const waitForSelectedAgent = async (
    profileId: string,
    defaultAgentId: string | null,
  ) => {
    const deadline = Date.now() + SAVE_CONFIRMATION_TIMEOUT_MS;
    while (Date.now() < deadline) {
      const snapshot = await actions.readSnapshot();
      const deployment = snapshot.client?.deployments.find(
        (candidate) => candidate.nodeDid === setupNodeDid,
      );
      const bound = deployment?.agentConfigs.find(
        (agent) =>
          agent.agent_id === defaultAgentId && agent.inference_profile_id === profileId,
      );
      const ready = deployment?.nodeReadiness.agents.some(
        (status) => status.state === "ready" && status.agentId === defaultAgentId,
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
      const { profileId, defaultAgentId } = await persistInference();
      const snapshot = await waitForSelectedAgent(profileId, defaultAgentId);
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
        <Title note="Provider sign-in and the saved configuration are stored by your local node.">
          {purpose === "add-backend"
            ? "Add an inference backend"
            : "Choose an inference provider"}
        </Title>
        {runtimeGate === "unavailable" ? (
          <div className="grid gap-3">
            <p role="alert" className="text-sm text-destructive">
              {error ??
                "The local node is not running, so provider sign-in is unavailable."}
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
              <Spinner /> Starting your local node…
            </p>
            {managedWait ? (
              <ManagedServerWaitNotice
                wait={managedWait}
                testId="setup-managed-server-wait"
                loginItemsTestId="setup-open-login-items"
                onOpenLoginItems={
                  actions.localServerOffers.loginItems
                    ? actions.openLocalServerLoginItems
                    : undefined
                }
              />
            ) : null}
          </div>
        )}
        <Nav onBack={onCancel} />
      </Frame>
    );
  }
  const providerDetails = (
    <div className="grid gap-4">
      <ConnectionFields
        form={form}
        dispatch={dispatch}
        provider={provider}
        connection={connection}
        option={providerOption}
        purpose={purpose}
        canRetrySave={actions.canRetryProviderSave}
        ops={{
          updateConnection,
          signIn: () => void signIn(),
          cancelSignIn,
          useAnotherAccount,
          retrySave: () => void retrySaveSignIn(),
          discover: () => void discoverModels(),
        }}
      />
      <ModelChoice
        choice={form.model}
        busy={busy}
        dispatch={dispatch}
        purpose={purpose}
        onDiscover={() => void discoverModels()}
        onDescribe={() => void describeManualModel()}
        onSave={() => void saveInference()}
      />
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
