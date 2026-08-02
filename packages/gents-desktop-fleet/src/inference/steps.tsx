import type { Detection, WizardStep } from "./constants.js";

export function ChooseStep({
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
      <OptionCard
        testid="inference-option-grok"
        title="Grok subscription (SuperGrok / X Premium+)"
        meta="Sign in with your Grok / xAI account"
        onPick={() => onPick("grok")}
      />
    </ul>
  );
}

export function OpenAiStep({
  apiKey,
  model,
  submitting,
  onApiKeyChange,
  onModelChange,
  onSubmit,
}: {
  apiKey: string;
  model: string;
  submitting: boolean;
  onApiKeyChange: (value: string) => void;
  onModelChange: (value: string) => void;
  onSubmit: () => void;
}) {
  return (
    <div className="inference-wizard-form">
      <label className="field">
        <span>OpenAI API key</span>
        <input
          autoFocus
          data-testid="inference-openai-key"
          placeholder="sk-…"
          type="password"
          value={apiKey}
          onChange={(event) => onApiKeyChange(event.currentTarget.value)}
        />
      </label>
      <label className="field">
        <span>Model</span>
        <input
          data-testid="inference-openai-model"
          value={model}
          onChange={(event) => onModelChange(event.currentTarget.value)}
        />
      </label>
      <p className="muted small">
        The key is stored in the backend document on this agent.
      </p>
      <div className="inference-wizard-actions">
        <button
          className="primary-button"
          data-testid="inference-openai-save"
          disabled={submitting || !apiKey.trim() || !model.trim()}
          onClick={onSubmit}
          type="button"
        >
          {submitting ? "Saving…" : "Save"}
        </button>
      </div>
    </div>
  );
}

export function LocalStep({
  detection,
  model,
  submitting,
  url,
  onDetect,
  onModelChange,
  onSubmit,
  onUrlChange,
}: {
  detection: Detection;
  model: string;
  submitting: boolean;
  url: string;
  onDetect: () => void;
  onModelChange: (value: string) => void;
  onSubmit: () => void;
  onUrlChange: (value: string) => void;
}) {
  return (
    <div className="inference-wizard-form">
      <label className="field">
        <span>Local server base URL</span>
        <div className="inference-wizard-inline">
          <input
            data-testid="inference-local-url"
            value={url}
            onChange={(event) => onUrlChange(event.currentTarget.value)}
          />
          <button
            className="ghost-button"
            disabled={submitting || detection.status === "probing"}
            onClick={onDetect}
            type="button"
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
            value={model}
            onChange={(event) => onModelChange(event.currentTarget.value)}
          >
            {detection.models.map((entry) => (
              <option key={entry} value={entry}>
                {entry}
              </option>
            ))}
          </select>
        </label>
      ) : (
        <label className="field">
          <span>Model name</span>
          <input
            data-testid="inference-local-model"
            value={model}
            onChange={(event) => onModelChange(event.currentTarget.value)}
          />
        </label>
      )}
      <div className="inference-wizard-actions">
        <button
          className="primary-button"
          data-testid="inference-local-save"
          disabled={submitting || !url.trim() || !model.trim()}
          onClick={onSubmit}
          type="button"
        >
          {submitting ? "Saving…" : "Save"}
        </button>
      </div>
    </div>
  );
}

export function CustomStep({
  apiKey,
  model,
  submitting,
  url,
  onApiKeyChange,
  onModelChange,
  onSubmit,
  onUrlChange,
}: {
  apiKey: string;
  model: string;
  submitting: boolean;
  url: string;
  onApiKeyChange: (value: string) => void;
  onModelChange: (value: string) => void;
  onSubmit: () => void;
  onUrlChange: (value: string) => void;
}) {
  return (
    <div className="inference-wizard-form">
      <label className="field">
        <span>Backend base URL (incl. /v1)</span>
        <input
          autoFocus
          data-testid="inference-custom-url"
          placeholder="https://…/v1"
          value={url}
          onChange={(event) => onUrlChange(event.currentTarget.value)}
        />
      </label>
      <label className="field">
        <span>Model name</span>
        <input
          data-testid="inference-custom-model"
          value={model}
          onChange={(event) => onModelChange(event.currentTarget.value)}
        />
      </label>
      <label className="field">
        <span>API key (optional)</span>
        <input
          data-testid="inference-custom-key"
          type="password"
          value={apiKey}
          onChange={(event) => onApiKeyChange(event.currentTarget.value)}
        />
      </label>
      <div className="inference-wizard-actions">
        <button
          className="primary-button"
          data-testid="inference-custom-save"
          disabled={submitting || !url.trim() || !model.trim()}
          onClick={onSubmit}
          type="button"
        >
          {submitting ? "Saving…" : "Save"}
        </button>
      </div>
    </div>
  );
}

export function CodexStep({
  authUrl,
  signingIn,
  submitting,
  onCancel,
  onSubmit,
}: {
  authUrl: string | null;
  signingIn: boolean;
  submitting: boolean;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <div className="inference-wizard-form">
      <p className="muted">
        Use your ChatGPT subscription. A browser window opens for you to sign
        in; the credential is stored on this agent and refreshed automatically.
      </p>
      {submitting && authUrl ? (
        <p className="muted small">
          Didn’t the browser open?{" "}
          <span className="mono inference-wizard-authurl">{authUrl}</span>
        </p>
      ) : null}
      <div className="inference-wizard-actions">
        {signingIn ? (
          <button
            className="ghost-button"
            data-testid="inference-codex-cancel"
            onClick={onCancel}
            type="button"
          >
            Cancel sign-in
          </button>
        ) : null}
        <button
          className="primary-button"
          data-testid="inference-codex-signin"
          disabled={submitting}
          onClick={onSubmit}
          type="button"
        >
          {submitting ? "Waiting for sign-in…" : "Sign in with ChatGPT"}
        </button>
      </div>
    </div>
  );
}

export function GrokStep({
  authUrl,
  signingIn,
  submitting,
  onCancel,
  onSubmit,
}: {
  authUrl: string | null;
  signingIn: boolean;
  submitting: boolean;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <div className="inference-wizard-form">
      <p className="muted">
        Use SuperGrok or an eligible X Premium+ subscription. Open the device
        code URL, approve access, and the credential is stored on this agent
        (refreshed automatically; no console.x.ai API key).
      </p>
      {submitting && authUrl ? (
        <p className="muted small">
          Open this URL to finish sign-in:{" "}
          <span className="mono inference-wizard-authurl">{authUrl}</span>
        </p>
      ) : null}
      <div className="inference-wizard-actions">
        {signingIn ? (
          <button
            className="ghost-button"
            data-testid="inference-grok-cancel"
            onClick={onCancel}
            type="button"
          >
            Cancel sign-in
          </button>
        ) : null}
        <button
          className="primary-button"
          data-testid="inference-grok-signin"
          disabled={submitting}
          onClick={onSubmit}
          type="button"
        >
          {submitting ? "Waiting for sign-in…" : "Sign in with Grok"}
        </button>
      </div>
    </div>
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
        onClick={onPick}
        type="button"
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
