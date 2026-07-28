import { useCallback, useEffect, useRef, useState } from "react";

import type {
  BackendSaveRequest,
  BehaviorSaveRequest,
  BehaviorView,
  CodexLoginResult,
  DeploymentView,
  InferenceProbeResult,
} from "@source-inc/gents-desktop-client";

// The same defaults the CLI init picker resolves onto, so the two onboarding
// paths land the user on identical backend documents.
const OPENAI_ENDPOINT = "https://api.openai.com/v1";
const OPENAI_DEFAULT_MODEL = "gpt-5.4-mini";
const OLLAMA_DEFAULT_URL = "http://127.0.0.1:11434/v1";
const LOCAL_PROBE_URLS = [
  "http://127.0.0.1:8080/v1",
  "http://127.0.0.1:11434/v1",
];
const CODEX_ENDPOINT = "https://chatgpt.com/backend-api/codex";
const CODEX_DEFAULT_MODEL = "gpt-5.5";

// Runtime-canonical provider kinds (see gents::backend_provider).
const PROVIDER_OPENAI = "openai";
const PROVIDER_CODEX = "ChatGptCodex";

// Wire API per OpenAI-compatible target (gents::openai_wire). OpenAI itself
// speaks the Responses API — matching the CLI preset — while local/self-hosted
// OpenAI-compatible servers speak chat completions.
const WIRE_RESPONSES = "responses";
const WIRE_CHAT_COMPLETIONS = "chat_completions";

type WizardStep = "choose" | "openai" | "local" | "custom" | "codex";

type Detection = {
  status: "idle" | "probing" | "found" | "none";
  url: string;
  models: string[];
};

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

/** The backend + behavior the wizard reconfigures for an already-provisioned agent. */
function resolveTargets(deployment: DeploymentView) {
  const behavior =
    deployment.behaviors.find((entry) => entry.isDefault) ??
    deployment.behaviors[0] ??
    null;
  const backend =
    deployment.inferenceBackends.find(
      (entry) => entry.backendId === behavior?.backendId,
    ) ??
    deployment.inferenceBackends[0] ??
    null;
  const backendId = backend?.backendId ?? behavior?.backendId ?? "default";
  return { behavior, backend, backendId };
}

/** Re-save a behavior unchanged except for its backend link; the desktop
 *  save path re-derives `model_name` from the backend's first model, which is
 *  how the chosen model actually reaches the behavior. */
function behaviorSaveFrom(
  behavior: BehaviorView,
  agentDid: string,
  backendId: string,
): BehaviorSaveRequest {
  return {
    agentDid,
    behaviorId: behavior.behaviorId,
    displayName: behavior.displayName,
    systemPrompt: behavior.systemPrompt ?? "",
    backendId,
    toolSelectionId: behavior.toolSelectionId ?? null,
    inferenceProfileId: behavior.inferenceProfileId ?? null,
    compactionStrategy: behavior.compactionStrategy ?? null,
    compactionThreshold: behavior.compactionThreshold ?? null,
    enabled: behavior.enabled,
    skillRefs: behavior.skillRefs,
    skillExcludes: behavior.skillExcludes,
  };
}

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

  async function persistBackend(opts: {
    name: string;
    providerKind: string;
    endpoint: string;
    models: string[];
    apiKey?: string;
    clearApiKey?: boolean;
    openaiWireApi?: string;
  }) {
    await onSaveBackendConfig({
      backendId: targets.backendId,
      name: opts.name,
      providerKind: opts.providerKind,
      openaiWireApi: opts.openaiWireApi,
      endpoint: opts.endpoint,
      apiKey: opts.apiKey,
      // Preserve any concurrency tuning already on the backend — the save path
      // overwrites these with whatever we send.
      maxConcurrent: targets.backend?.maxConcurrent ?? undefined,
      maxQueueDepth: targets.backend?.maxQueueDepth ?? undefined,
      clearApiKey: opts.clearApiKey ?? false,
      models: opts.models,
      enabled: true,
    });
    // Re-save the behavior so it re-derives its model from the updated backend
    // and points at it. Without this, the behavior keeps its stale model.
    if (targets.behavior) {
      await onSaveBehaviorConfig(
        behaviorSaveFrom(
          targets.behavior,
          deployment.agentDid,
          targets.backendId,
        ),
      );
    }
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
            <div className="inference-wizard-form">
              <label className="field">
                <span>OpenAI API key</span>
                <input
                  data-testid="inference-openai-key"
                  type="password"
                  autoFocus
                  placeholder="sk-…"
                  value={openaiKey}
                  onChange={(event) => setOpenaiKey(event.currentTarget.value)}
                />
              </label>
              <label className="field">
                <span>Model</span>
                <input
                  data-testid="inference-openai-model"
                  value={openaiModel}
                  onChange={(event) =>
                    setOpenaiModel(event.currentTarget.value)
                  }
                />
              </label>
              <p className="muted small">
                The key is stored in the backend document on this agent.
              </p>
              <div className="inference-wizard-actions">
                <button
                  className="primary-button"
                  data-testid="inference-openai-save"
                  type="button"
                  disabled={
                    submitting || !openaiKey.trim() || !openaiModel.trim()
                  }
                  onClick={() => void submitOpenai()}
                >
                  {submitting ? "Saving…" : "Save"}
                </button>
              </div>
            </div>
          ) : null}

          {!done && step === "local" ? (
            <div className="inference-wizard-form">
              <label className="field">
                <span>Local server base URL</span>
                <div className="inference-wizard-inline">
                  <input
                    data-testid="inference-local-url"
                    value={localUrl}
                    onChange={(event) => setLocalUrl(event.currentTarget.value)}
                  />
                  <button
                    className="ghost-button"
                    type="button"
                    disabled={submitting || detection.status === "probing"}
                    onClick={() => void reprobeLocal()}
                  >
                    {detection.status === "probing" ? "Detecting…" : "Detect"}
                  </button>
                </div>
              </label>
              <LocalDetectionHint detection={detection} />
              {detection.status === "found" && detection.models.length > 0 ? (
                <label className="field">
                  <span>Model</span>
                  <select
                    data-testid="inference-local-model"
                    value={localModel}
                    onChange={(event) =>
                      setLocalModel(event.currentTarget.value)
                    }
                  >
                    {detection.models.map((model) => (
                      <option key={model} value={model}>
                        {model}
                      </option>
                    ))}
                  </select>
                </label>
              ) : (
                <label className="field">
                  <span>Model name</span>
                  <input
                    data-testid="inference-local-model"
                    value={localModel}
                    onChange={(event) =>
                      setLocalModel(event.currentTarget.value)
                    }
                  />
                </label>
              )}
              <div className="inference-wizard-actions">
                <button
                  className="primary-button"
                  data-testid="inference-local-save"
                  type="button"
                  disabled={
                    submitting || !localUrl.trim() || !localModel.trim()
                  }
                  onClick={() => void submitLocal()}
                >
                  {submitting ? "Saving…" : "Save"}
                </button>
              </div>
            </div>
          ) : null}

          {!done && step === "custom" ? (
            <div className="inference-wizard-form">
              <label className="field">
                <span>Backend base URL (incl. /v1)</span>
                <input
                  data-testid="inference-custom-url"
                  autoFocus
                  placeholder="https://…/v1"
                  value={customUrl}
                  onChange={(event) => setCustomUrl(event.currentTarget.value)}
                />
              </label>
              <label className="field">
                <span>Model name</span>
                <input
                  data-testid="inference-custom-model"
                  value={customModel}
                  onChange={(event) =>
                    setCustomModel(event.currentTarget.value)
                  }
                />
              </label>
              <label className="field">
                <span>API key (optional)</span>
                <input
                  data-testid="inference-custom-key"
                  type="password"
                  value={customKey}
                  onChange={(event) => setCustomKey(event.currentTarget.value)}
                />
              </label>
              <div className="inference-wizard-actions">
                <button
                  className="primary-button"
                  data-testid="inference-custom-save"
                  type="button"
                  disabled={
                    submitting || !customUrl.trim() || !customModel.trim()
                  }
                  onClick={() => void submitCustom()}
                >
                  {submitting ? "Saving…" : "Save"}
                </button>
              </div>
            </div>
          ) : null}

          {!done && step === "codex" ? (
            <div className="inference-wizard-form">
              <p className="muted">
                Use your ChatGPT subscription. A browser window opens for you to
                sign in; the credential is stored on this agent and refreshed
                automatically.
              </p>
              {submitting && codexAuthUrl ? (
                <p className="muted small">
                  Didn’t the browser open?{" "}
                  <span className="mono inference-wizard-authurl">
                    {codexAuthUrl}
                  </span>
                </p>
              ) : null}
              <div className="inference-wizard-actions">
                {signingIn ? (
                  <button
                    className="ghost-button"
                    data-testid="inference-codex-cancel"
                    type="button"
                    onClick={cancelAndClose}
                  >
                    Cancel sign-in
                  </button>
                ) : null}
                <button
                  className="primary-button"
                  data-testid="inference-codex-signin"
                  type="button"
                  disabled={submitting}
                  onClick={() => void signInWithChatGpt()}
                >
                  {submitting ? "Waiting for sign-in…" : "Sign in with ChatGPT"}
                </button>
              </div>
            </div>
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

function ChooseStep({
  detection,
  onPick,
}: {
  detection: Detection;
  onPick: (step: WizardStep) => void;
}) {
  const localMeta =
    detection.status === "found"
      ? `Detected at ${detection.url}`
      : detection.status === "probing"
        ? "Looking for a local server…"
        : "e.g. Ollama / llama-server";
  return (
    <ul
      className="inference-wizard-options"
      data-testid="inference-wizard-options"
    >
      <OptionCard
        testid="inference-option-openai"
        title="OpenAI API key"
        meta="Paste a key; stored in the backend document"
        onPick={() => onPick("openai")}
      />
      <OptionCard
        testid="inference-option-local"
        title="Local server"
        meta={localMeta}
        onPick={() => onPick("local")}
      />
      <OptionCard
        testid="inference-option-custom"
        title="Custom URL"
        meta="Any OpenAI-compatible endpoint"
        onPick={() => onPick("custom")}
      />
      <OptionCard
        testid="inference-option-codex"
        title="ChatGPT / Codex subscription"
        meta="Sign in with your ChatGPT plan"
        onPick={() => onPick("codex")}
      />
    </ul>
  );
}

function OptionCard({
  testid,
  title,
  meta,
  onPick,
}: {
  testid: string;
  title: string;
  meta: string;
  onPick: () => void;
}) {
  return (
    <li>
      <button
        className="inference-wizard-option"
        data-testid={testid}
        type="button"
        onClick={onPick}
      >
        <span className="inference-wizard-option-title">{title}</span>
        <span className="inference-wizard-option-meta muted">{meta}</span>
      </button>
    </li>
  );
}

function LocalDetectionHint({ detection }: { detection: Detection }) {
  if (detection.status === "found") {
    return (
      <p className="muted small">
        Detected a running server at {detection.url}.
      </p>
    );
  }
  if (detection.status === "none") {
    return (
      <p className="muted small">
        No local server detected. Start one (Ollama or llama-server), then
        Detect.
      </p>
    );
  }
  return null;
}
