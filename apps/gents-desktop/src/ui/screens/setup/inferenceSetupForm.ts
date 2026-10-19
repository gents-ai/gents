import type {
  InferenceAuthMethod,
  InferenceDiscoveryResult,
  InferenceModelOption,
  InferenceModelRecommendation,
  InferenceProviderId,
  InferenceProviderOption,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { PROVIDER_CREDENTIAL_KIND, type OauthProvider } from "@/lib/providerLogin";
import {
  recommendedInferenceSettings,
  type InferenceSettingsDraft,
} from "../inference/InferenceModelControls";

export type SignedIn = { credentialId: string; accountRef: string | null };

export type ProviderId = InferenceProviderId;

export type ConnectionDraft = {
  authMethod: InferenceAuthMethod;
  endpoint: string;
  apiKey: string;
};

export type SetupOp = "signIn" | "retrySave" | "discover" | "describe" | "save";

export type RuntimeGate = "idle" | "checking" | "ready" | "unavailable";

type ModelChoice = {
  discovery: InferenceDiscoveryResult | null;
  search: string;
  pickerOpen: boolean;
  name: string;
  manual: boolean;
  recommendation: InferenceModelRecommendation | null;
  settings: InferenceSettingsDraft | null;
  customize: boolean;
};

export type SetupForm = {
  op: SetupOp | null;
  error: string | null;
  provider: ProviderId | null;
  connections: Partial<Record<ProviderId, ConnectionDraft>>;
  runtimeGate: RuntimeGate;
  accounts: {
    stored: readonly ProviderAccountView[];
    signedIn: Partial<Record<ProviderId, SignedIn>>;
    pendingSave: Partial<Record<ProviderId, true>>;
    switching: Partial<Record<ProviderId, true>>;
    observed: boolean;
  };
  accountLabel: string;
  signInHint: string | null;
  authUrl: string | null;
  autoSignIn: { provider: ProviderId; asked: number } | null;
  model: ModelChoice;
};

export type SetupEvent =
  | { type: "opStarted"; op: SetupOp }
  | { type: "opEnded" }
  | { type: "failed"; error: string | null }
  | { type: "providerPicked"; provider: ProviderId; current: ProviderId }
  | {
      type: "connectionEdited";
      provider: ProviderId;
      connection: ConnectionDraft;
      autoSignIn?: boolean;
    }
  | { type: "runtimeGate"; gate: RuntimeGate; error?: string }
  | { type: "accountsCleared" }
  | {
      type: "accountsRead";
      accounts: readonly ProviderAccountView[];
      seedSignedIn: boolean;
    }
  | { type: "pendingSaveCleared"; provider: ProviderId }
  | { type: "connected"; provider: ProviderId; account: SignedIn }
  | { type: "signedOut"; provider: ProviderId }
  | { type: "autoSignInTaken" }
  | { type: "authUrl"; url: string | null }
  | { type: "hint"; hint: string | null }
  | { type: "labelEdited"; label: string }
  | { type: "modelsDiscovered"; discovery: InferenceDiscoveryResult }
  | { type: "modelChosen"; option: InferenceModelOption }
  | { type: "manualModelTyped"; name: string }
  | { type: "recommendationLoaded"; recommendation: InferenceModelRecommendation }
  | { type: "settingsEdited"; settings: InferenceSettingsDraft }
  | { type: "customizeToggled"; expanded: boolean }
  | { type: "searchEdited"; search: string }
  | { type: "pickerOpened" };

export function initialSetupForm(fixedProvider?: ProviderId): SetupForm {
  return {
    op: null,
    error: null,
    provider: fixedProvider ?? null,
    connections: {},
    runtimeGate: "idle",
    accounts: {
      stored: [],
      signedIn: {},
      pendingSave: {},
      switching: {},
      observed: false,
    },
    accountLabel: "",
    signInHint: null,
    authUrl: null,
    autoSignIn: fixedProvider ? { provider: fixedProvider, asked: 1 } : null,
    model: {
      discovery: null,
      search: "",
      pickerOpen: true,
      name: "",
      manual: false,
      recommendation: null,
      settings: null,
      customize: false,
    },
  };
}

const modelCleared = (model: ModelChoice): ModelChoice => ({
  ...model,
  discovery: null,
  name: "",
  manual: false,
  recommendation: null,
  settings: null,
  customize: false,
});

const asked = (form: SetupForm, provider: ProviderId) => ({
  provider,
  asked: (form.autoSignIn?.asked ?? 0) + 1,
});

function without<T>(state: Partial<Record<ProviderId, T>>, id: ProviderId) {
  const next = { ...state };
  delete next[id];
  return next;
}

function withoutAll<T>(
  state: Partial<Record<ProviderId, T>>,
  ids: Partial<Record<ProviderId, unknown>>,
) {
  const next = { ...state };
  for (const id of Object.keys(ids) as ProviderId[]) delete next[id];
  return next;
}

export function setupFormReducer(form: SetupForm, event: SetupEvent): SetupForm {
  switch (event.type) {
    case "opStarted":
      return event.op === "signIn"
        ? { ...form, op: event.op, error: null, authUrl: null, signInHint: null }
        : { ...form, op: event.op, error: null };
    case "opEnded":
      return { ...form, op: null };
    case "failed":
      return { ...form, error: event.error };
    case "providerPicked":
      if (event.provider === event.current)
        return { ...form, autoSignIn: asked(form, event.provider) };
      return {
        ...form,
        provider: event.provider,
        autoSignIn: asked(form, event.provider),
        authUrl: null,
        error: null,
        model: modelCleared(form.model),
      };
    case "connectionEdited":
      return {
        ...form,
        connections: { ...form.connections, [event.provider]: event.connection },
        autoSignIn: event.autoSignIn ? asked(form, event.provider) : form.autoSignIn,
        error: null,
        model: modelCleared(form.model),
      };
    case "runtimeGate":
      return {
        ...form,
        runtimeGate: event.gate,
        error:
          event.gate === "checking"
            ? null
            : event.error !== undefined
              ? event.error
              : form.error,
      };
    case "accountsCleared":
      return {
        ...form,
        accounts: {
          ...form.accounts,
          signedIn: {},
          pendingSave: {},
          switching: {},
          observed: false,
        },
      };
    case "accountsRead":
      return {
        ...form,
        accounts: {
          stored: event.accounts,
          signedIn: event.seedSignedIn
            ? withoutAll(providerSignInState(event.accounts), form.accounts.switching)
            : form.accounts.signedIn,
          pendingSave: providerPendingSaveState(event.accounts),
          switching: form.accounts.switching,
          observed: true,
        },
      };
    case "pendingSaveCleared":
      return {
        ...form,
        accounts: {
          ...form.accounts,
          pendingSave: without(form.accounts.pendingSave, event.provider),
        },
      };
    case "connected":
      return {
        ...form,
        error: null,
        authUrl: null,
        accounts: {
          ...form.accounts,
          signedIn: { ...form.accounts.signedIn, [event.provider]: event.account },
          switching: without(form.accounts.switching, event.provider),
        },
        model: modelCleared(form.model),
      };
    case "signedOut":
      return {
        ...form,
        accounts: {
          ...form.accounts,
          signedIn: without(form.accounts.signedIn, event.provider),
          switching: { ...form.accounts.switching, [event.provider]: true },
        },
        model: modelCleared(form.model),
      };
    case "autoSignInTaken":
      return { ...form, autoSignIn: null };
    case "authUrl":
      return { ...form, authUrl: event.url };
    case "hint":
      return { ...form, signInHint: event.hint };
    case "labelEdited":
      return { ...form, accountLabel: event.label };
    case "modelsDiscovered":
      return {
        ...form,
        model: {
          ...form.model,
          discovery: event.discovery,
          name: "",
          recommendation: null,
          settings: null,
        },
      };
    case "modelChosen":
      return {
        ...form,
        model: {
          ...form.model,
          name: event.option.advertised.model_name,
          recommendation: event.option.recommendation,
          settings: recommendedInferenceSettings(event.option.recommendation),
          customize: false,
          pickerOpen: false,
        },
      };
    case "manualModelTyped":
      return {
        ...form,
        model: {
          ...form.model,
          name: event.name,
          manual: true,
          recommendation: null,
          settings: null,
        },
      };
    case "recommendationLoaded":
      return {
        ...form,
        model: {
          ...form.model,
          recommendation: event.recommendation,
          settings: recommendedInferenceSettings(event.recommendation),
        },
      };
    case "settingsEdited":
      return { ...form, model: { ...form.model, settings: event.settings } };
    case "customizeToggled":
      return { ...form, model: { ...form.model, customize: event.expanded } };
    case "searchEdited":
      return { ...form, model: { ...form.model, search: event.search } };
    case "pickerOpened":
      return { ...form, model: { ...form.model, pickerOpen: true } };
  }
}

export const connectionDefaults = (
  option: InferenceProviderOption,
): ConnectionDraft => ({
  authMethod: option.defaultAuthMethod,
  endpoint: option.defaultEndpoint,
  apiKey: "",
});

export function providerSignInState(accounts: readonly ProviderAccountView[]) {
  const next: Partial<Record<ProviderId, SignedIn>> = {};
  for (const [providerId, credentialKind] of Object.entries(PROVIDER_CREDENTIAL_KIND)) {
    const account = accounts.find(
      (entry) =>
        entry.enabled && !entry.pendingSave && entry.provider === credentialKind,
    );
    if (account)
      next[providerId as OauthProvider] = {
        credentialId: account.credentialId,
        accountRef: account.accountRef,
      };
  }
  return next;
}

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

export const oauthProviderFor = (method: InferenceAuthMethod): OauthProvider | null =>
  method === "chat_gpt_oauth"
    ? "openai"
    : method === "claude_oauth"
      ? "anthropic"
      : method === "grok_oauth"
        ? "grok"
        : null;
