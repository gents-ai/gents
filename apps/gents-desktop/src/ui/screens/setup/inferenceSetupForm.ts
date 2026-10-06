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

export type ProviderId = InferenceProviderId;

export type ConnectionDraft = {
  authMethod: InferenceAuthMethod;
  endpoint: string;
  apiKey: string;
};

/** What the form is waiting on; it is busy while there is one. */
export type SetupOp = "signIn" | "retrySave" | "discover" | "describe" | "save";

/** Whether a local agent's managed runtime, which sign-in and the save
    write through, is known to be serving. */
export type RuntimeGate = "idle" | "checking" | "ready" | "unavailable";

type ModelChoice = {
  discovery: InferenceDiscoveryResult | null;
  search: string;
  pickerOpen: boolean;
  name: string;
  /** typed by hand, where discovery allows it */
  manual: boolean;
  recommendation: InferenceModelRecommendation | null;
  settings: InferenceSettingsDraft | null;
  customize: boolean;
};

export type SetupForm = {
  op: SetupOp | null;
  error: string | null;
  /** the provider chosen; null until the person chooses, while the catalog's
      first stands */
  provider: ProviderId | null;
  /** each provider's connection as edited; one not edited is the catalog's
      defaults */
  connections: Partial<Record<ProviderId, ConnectionDraft>>;
  runtimeGate: RuntimeGate;
  accounts: {
    stored: readonly ProviderAccountView[];
    signedIn: Partial<Record<ProviderId, string>>;
    pendingSave: Partial<Record<ProviderId, true>>;
    /** a read has landed, so whether an account is connected is known */
    observed: boolean;
  };
  accountLabel: string;
  signInHint: string | null;
  authUrl: string | null;
  /** a provider the person just chose, whose sign-in starts by itself once
      it can; `asked` tells two choices of the same provider apart */
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
      /** a new connection method is a choice too: its sign-in starts by itself */
      autoSignIn?: boolean;
    }
  | { type: "runtimeGate"; gate: RuntimeGate; error?: string }
  | { type: "accountsCleared" }
  | {
      type: "accountsRead";
      accounts: readonly ProviderAccountView[];
      /** adding a backend signs in a further account; only its own sign-in
          connects */
      seedSignedIn: boolean;
    }
  | { type: "pendingSaveCleared"; provider: ProviderId }
  | { type: "connected"; provider: ProviderId; credentialId: string }
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
    accounts: { stored: [], signedIn: {}, pendingSave: {}, observed: false },
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

/* the connection changed, so what was discovered through the old one no
   longer applies; the search and the picker stay as the person left them */
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
      /* choosing the provider already shown is a choice too */
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
        accounts: { ...form.accounts, signedIn: {}, pendingSave: {}, observed: false },
      };
    case "accountsRead":
      return {
        ...form,
        accounts: {
          stored: event.accounts,
          signedIn: event.seedSignedIn
            ? providerSignInState(event.accounts)
            : form.accounts.signedIn,
          pendingSave: providerPendingSaveState(event.accounts),
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
        accounts: {
          ...form.accounts,
          signedIn: { ...form.accounts.signedIn, [event.provider]: event.credentialId },
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
      /* discovery supplies choices, but the person makes the one model
         decision: even a one-item catalog is never accepted implicitly */
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

/** A provider's connection before the person edits it: the catalog's
    default method and endpoint, and no key. */
export const connectionDefaults = (
  option: InferenceProviderOption,
): ConnectionDraft => ({
  authMethod: option.defaultAuthMethod,
  endpoint: option.defaultEndpoint,
  apiKey: "",
});

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

/** The provider whose sign-in a connection method is, if it is one. */
export const oauthProviderFor = (method: InferenceAuthMethod): OauthProvider | null =>
  method === "chat_gpt_oauth"
    ? "openai"
    : method === "claude_oauth"
      ? "anthropic"
      : method === "grok_oauth"
        ? "grok"
        : null;
