import { useCallback, useEffect, useState } from "react";

import type {
  BackendSaveRequest,
  BehaviorSaveRequest,
  CodexLoginResult,
  DeploymentView,
  InferenceProbeResult,
} from "@source-inc/gents-desktop-client";
import {
  CODEX_DEFAULT_MODEL,
  CODEX_ENDPOINT,
  LOCAL_PROBE_URLS,
  OLLAMA_DEFAULT_URL,
  OPENAI_DEFAULT_MODEL,
  OPENAI_ENDPOINT,
  PROVIDER_CODEX,
  PROVIDER_OPENAI,
  WIRE_CHAT_COMPLETIONS,
  WIRE_RESPONSES,
  type Detection,
  type WizardStep,
} from "./constants.js";
import {
  persistInferenceBackend,
  type PersistBackendOptions,
} from "./persistBackend.js";
import { resolveTargets } from "./resolveTargets.js";

export type InferenceSetupOptions = {
  deployment: DeploymentView;
  onClose: () => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onProbeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  onCodexLogin: (agentDid: string) => Promise<CodexLoginResult>;
  /** Abort a ChatGPT sign-in whose browser was closed, so it does not hang. */
  onCancelCodexLogin?: () => Promise<unknown>;
  /** Host adapter for the optional browser-login URL event. */
  onCodexLoginUrl?: (
    onUrl: (url: string | null) => void,
  ) => Promise<() => void>;
};

export function useInferenceSetup({
  deployment,
  onClose,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
  onProbeInferenceEndpoint,
  onCodexLogin,
  onCancelCodexLogin,
  onCodexLoginUrl,
}: InferenceSetupOptions) {
  const targets = resolveTargets(deployment);
  const [step, setStep] = useState<WizardStep>("choose");
  const [submitting, setSubmitting] = useState(false);
  // Only browser sign-in can wait indefinitely, so cancellation also aborts
  // the backend login server and frees its loopback port.
  const [signingIn, setSigningIn] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [openaiKey, setOpenaiKey] = useState("");
  const [openaiModel, setOpenaiModel] = useState(OPENAI_DEFAULT_MODEL);
  const [localUrl, setLocalUrl] = useState(OLLAMA_DEFAULT_URL);
  const [localModel, setLocalModel] = useState("");
  const [customUrl, setCustomUrl] = useState("");
  const [customModel, setCustomModel] = useState("");
  const [customKey, setCustomKey] = useState("");
  const [detection, setDetection] = useState<Detection>({
    status: "idle",
    url: "",
    models: [],
  });
  const [codexAuthUrl, setCodexAuthUrl] = useState<string | null>(null);
  const [codexResult, setCodexResult] = useState<CodexLoginResult | null>(null);

  // Best-effort first-run detection mirrors the CLI picker.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setDetection({ status: "probing", url: "", models: [] });
      for (const url of LOCAL_PROBE_URLS) {
        try {
          const result = await onProbeInferenceEndpoint(url);
          if (cancelled) return;
          if (result.reachable && result.models.length > 0) {
            setDetection({ status: "found", url, models: result.models });
            setLocalUrl(url);
            setLocalModel(result.models[0]);
            return;
          }
        } catch {
          // Keep probing the next candidate.
        }
      }
      if (!cancelled) setDetection({ status: "none", url: "", models: [] });
    })();
    return () => {
      cancelled = true;
    };
    // The shell recreates its action closures each render; the Detect button
    // handles explicit retries without restarting this mount-only probe.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const cancelAndClose = useCallback(() => {
    if (signingIn) {
      void Promise.resolve(onCancelCodexLogin?.()).catch(() => {
        // Best-effort for hosts without the optional cancel command.
      });
    }
    onClose();
  }, [signingIn, onCancelCodexLogin, onClose]);

  useEffect(() => {
    const handler = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") cancelAndClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [cancelAndClose]);

  function persistBackend(options: PersistBackendOptions) {
    return persistInferenceBackend({
      deployment,
      options,
      onSaveBackendConfig,
      onSaveBehaviorConfig,
    });
  }

  function backendName(fallback: string) {
    return targets.backend?.name ?? targets.backend?.backendId ?? fallback;
  }

  async function submitOpenai() {
    if (!openaiKey.trim() || !openaiModel.trim()) return;
    setSubmitting(true);
    setError(null);
    try {
      await persistBackend({
        name: backendName("OpenAI"),
        providerKind: PROVIDER_OPENAI,
        openaiWireApi: WIRE_RESPONSES,
        endpoint: OPENAI_ENDPOINT,
        models: [openaiModel.trim()],
        apiKey: openaiKey,
      });
      setDone(`OpenAI · ${openaiModel.trim()}`);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setSubmitting(false);
    }
  }

  async function submitLocal() {
    const url = localUrl.trim();
    const model = localModel.trim();
    if (!url || !model) return;
    setSubmitting(true);
    setError(null);
    try {
      await persistBackend({
        name: backendName("Local server"),
        providerKind: PROVIDER_OPENAI,
        openaiWireApi: WIRE_CHAT_COMPLETIONS,
        endpoint: url,
        models: [model],
        clearApiKey: true,
      });
      setDone(`${url} · ${model}`);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setSubmitting(false);
    }
  }

  async function reprobeLocal() {
    const url = localUrl.trim();
    if (!url) return;
    setDetection({ status: "probing", url, models: [] });
    try {
      const result = await onProbeInferenceEndpoint(url);
      if (result.reachable && result.models.length > 0) {
        setDetection({ status: "found", url, models: result.models });
        setLocalModel((current) => current || result.models[0]);
      } else {
        setDetection({ status: "none", url, models: [] });
      }
    } catch {
      setDetection({ status: "none", url, models: [] });
    }
  }

  async function submitCustom() {
    const url = customUrl.trim();
    const model = customModel.trim();
    if (!url || !model) return;
    setSubmitting(true);
    setError(null);
    try {
      await persistBackend({
        name: backendName("Custom backend"),
        providerKind: PROVIDER_OPENAI,
        openaiWireApi: WIRE_CHAT_COMPLETIONS,
        endpoint: url,
        models: [model],
        apiKey: customKey.trim() ? customKey : undefined,
        clearApiKey: !customKey.trim(),
      });
      setDone(`${url} · ${model}`);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setSubmitting(false);
    }
  }

  async function signInWithChatGpt() {
    setSubmitting(true);
    setSigningIn(true);
    setError(null);
    setCodexAuthUrl(null);
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await onCodexLoginUrl?.(setCodexAuthUrl);
      const result = await onCodexLogin(deployment.agentDid);
      setCodexResult(result);
      await persistBackend({
        name: backendName("ChatGPT / Codex"),
        providerKind: PROVIDER_CODEX,
        endpoint: CODEX_ENDPOINT,
        models: [CODEX_DEFAULT_MODEL],
        clearApiKey: true,
      });
      setDone(`ChatGPT / Codex · ${CODEX_DEFAULT_MODEL}`);
    } catch (caught) {
      setError(String(caught));
    } finally {
      unlisten?.();
      setCodexAuthUrl(null);
      setSigningIn(false);
      setSubmitting(false);
    }
  }

  function backToOptions() {
    setError(null);
    setStep("choose");
  }

  return {
    codexAuthUrl,
    codexResult,
    customKey,
    customModel,
    customUrl,
    detection,
    done,
    error,
    localModel,
    localUrl,
    openaiKey,
    openaiModel,
    signingIn,
    step,
    submitting,
    backToOptions,
    cancelAndClose,
    reprobeLocal,
    setCustomKey,
    setCustomModel,
    setCustomUrl,
    setLocalModel,
    setLocalUrl,
    setOpenaiKey,
    setOpenaiModel,
    setStep,
    signInWithChatGpt,
    submitCustom,
    submitLocal,
    submitOpenai,
  };
}

export type InferenceSetupController = ReturnType<typeof useInferenceSetup>;
