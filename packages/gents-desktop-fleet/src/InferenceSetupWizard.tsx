import { useCallback, useEffect, useRef, useState } from "react";

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
} from "./inference/constants.js";
import { resolveTargets } from "./inference/resolveTargets.js";
import {
  persistInferenceBackend,
  type PersistBackendOptions,
} from "./inference/persistBackend.js";
import {
  ChooseStep,
  CodexStep,
  CustomStep,
  LocalStep,
  OpenAiStep,
} from "./inference/steps.js";

export type InferenceSetupWizardProps = {
  deployment: DeploymentView;
  onClose: () => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onProbeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  onCodexLogin: (agentDid: string) => Promise<CodexLoginResult>;
  // Abort a ChatGPT sign-in whose browser was closed, so it does not hang.
  // Optional so older callers and tests need not provide it.
  onCancelCodexLogin?: () => Promise<unknown>;
  /** Host adapter for the optional browser-login URL event. */
  onCodexLoginUrl?: (
    onUrl: (url: string | null) => void,
  ) => Promise<() => void>;
};

export function InferenceSetupWizard({
  deployment,
  onClose,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
  onProbeInferenceEndpoint,
  onCodexLogin,
  onCancelCodexLogin,
  onCodexLoginUrl,
}: InferenceSetupWizardProps) {
  const targets = resolveTargets(deployment);
  const dialogRef = useRef<HTMLDivElement | null>(null);

  const [step, setStep] = useState<WizardStep>("choose");
  const [submitting, setSubmitting] = useState(false);
  // True only while a ChatGPT sign-in is awaiting the browser callback — the
  // one submit that can hang, so cancelling it must also abort the backend.
  const [signingIn, setSigningIn] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  // OpenAI / custom / local form fields.
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

  // Auto-detect a running local server on the well-known ports, mirroring the
  // CLI picker's first-run probe. Best-effort; failure just means "not found".
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
    // Probe once when the wizard opens. The shell recreates its action closures
    // each render, so depending on the prop would re-probe on every re-render;
    // the Detect button covers manual re-detection.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Always dismissable: a sign-in whose browser was closed would otherwise
  // wait forever. Aborting a pending sign-in also stops the backend login
  // server so its loopback port is freed for the next attempt.
  const cancelAndClose = useCallback(() => {
    if (signingIn) {
      void Promise.resolve(onCancelCodexLogin?.()).catch(() => {
        // Best-effort — a build without the cancel command still closes here.
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
    } catch (err) {
      setError(String(err));
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
        // A local server needs no key; drop any stale one from a prior provider.
        clearApiKey: true,
      });
      setDone(`${url} · ${model}`);
    } catch (err) {
      setError(String(err));
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
    } catch (err) {
      setError(String(err));
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
      // Surface the auth URL as a manual-open fallback in case the system
      // browser did not launch on its own.
      unlisten = await onCodexLoginUrl?.(setCodexAuthUrl);
      // Log in first so a cancelled sign-in never leaves the agent pointed at a
      // Codex backend with no credential.
      const result = await onCodexLogin(deployment.agentDid);
      setCodexResult(result);
      await persistBackend({
        name: backendName("ChatGPT / Codex"),
        providerKind: PROVIDER_CODEX,
        endpoint: CODEX_ENDPOINT,
        models: [CODEX_DEFAULT_MODEL],
        // OAuth credential replaces any API key on this backend.
        clearApiKey: true,
      });
      setDone(`ChatGPT / Codex · ${CODEX_DEFAULT_MODEL}`);
    } catch (err) {
      setError(String(err));
    } finally {
      unlisten?.();
      setCodexAuthUrl(null);
      setSigningIn(false);
      setSubmitting(false);
    }
  }

  return (
    <div
      className="dialog-backdrop open"
      role="presentation"
      onClick={cancelAndClose}
    >
      <div
        className="dialog inference-wizard"
        role="dialog"
        aria-modal="true"
        aria-labelledby="inference-wizard-title"
        data-testid="inference-wizard"
        ref={dialogRef}
        onClick={(event) => event.stopPropagation()}
      >
        <header className="inference-wizard-header">
          <h3 id="inference-wizard-title">Set up inference</h3>
          <p className="muted">
            Choose how <strong>{deployment.label}</strong> runs model inference.
          </p>
        </header>

        <div className="inference-wizard-body">
          {done ? (
            <div
              className="inference-wizard-done"
              data-testid="inference-wizard-done"
            >
              <p className="inference-wizard-success">Inference is set up.</p>
              <p className="muted">{done}</p>
              {codexResult ? (
                <p className="muted">
                  Signed in
                  {codexResult.chatgptPlanType
                    ? ` · ${codexResult.chatgptPlanType} plan`
                    : ""}
                  {codexResult.accountId ? ` · ${codexResult.accountId}` : ""}
                </p>
              ) : null}
            </div>
          ) : step === "choose" ? (
            <ChooseStep detection={detection} onPick={setStep} />
          ) : (
            <button
              className="inference-wizard-back"
              type="button"
              disabled={submitting}
              onClick={() => {
                setError(null);
                setStep("choose");
              }}
            >
              ← Back to options
            </button>
          )}

          {error ? (
            <p
              className="inference-wizard-error"
              data-testid="inference-wizard-error"
            >
              {error}
            </p>
          ) : null}

          {!done && step === "openai" ? (
            <OpenAiStep
              apiKey={openaiKey}
              model={openaiModel}
              submitting={submitting}
              onApiKeyChange={setOpenaiKey}
              onModelChange={setOpenaiModel}
              onSubmit={() => void submitOpenai()}
            />
          ) : null}

          {!done && step === "local" ? (
            <LocalStep
              detection={detection}
              model={localModel}
              submitting={submitting}
              url={localUrl}
              onDetect={() => void reprobeLocal()}
              onModelChange={setLocalModel}
              onSubmit={() => void submitLocal()}
              onUrlChange={setLocalUrl}
            />
          ) : null}

          {!done && step === "custom" ? (
            <CustomStep
              apiKey={customKey}
              model={customModel}
              submitting={submitting}
              url={customUrl}
              onApiKeyChange={setCustomKey}
              onModelChange={setCustomModel}
              onSubmit={() => void submitCustom()}
              onUrlChange={setCustomUrl}
            />
          ) : null}

          {!done && step === "codex" ? (
            <CodexStep
              authUrl={codexAuthUrl}
              signingIn={signingIn}
              submitting={submitting}
              onCancel={cancelAndClose}
              onSubmit={() => void signInWithChatGpt()}
            />
          ) : null}
        </div>

        <footer className="inference-wizard-footer">
          <button
            className="ghost-button"
            data-testid="inference-wizard-close"
            type="button"
            onClick={cancelAndClose}
          >
            {done ? "Done" : "Cancel"}
          </button>
        </footer>
      </div>
    </div>
  );
}
