import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  BackendSaveRequest,
  DeploymentView,
  InferenceBackendView,
} from "../../lib/types";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import {
  ignoreHandledActionError,
  isOptionalInt,
  linesToArray,
  parseOptionalInt,
} from "./formUtils";

export type BackendConfigPanelProps = {
  deployment: DeploymentView;
  selectedBackendId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectBackend: (backendId: string) => void;
  onCreateBackend: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
};

export function BackendConfigPanel({
  deployment,
  selectedBackendId,
  saving,
  savedStatus,
  onSelectBackend,
  onCreateBackend,
  onSavedStatusChange,
  onSaveBackendConfig,
}: BackendConfigPanelProps) {
  const selectedBackend = useMemo(
    () =>
      deployment.inferenceBackends.find(
        (backend) => backend.backendId === selectedBackendId,
      ) ?? null,
    [deployment.inferenceBackends, selectedBackendId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Inference"
        items={deployment.inferenceBackends.map((backend) => ({
          id: backend.backendId,
          title: backend.name ?? backend.backendId,
          meta: backend.models[0] ?? backend.providerKind ?? "backend",
        }))}
        selectedId={selectedBackendId}
        testPrefix="backend"
        title="Backends"
        onCreate={onCreateBackend}
        onSelect={onSelectBackend}
      />

      <BackendConfigEditor
        backend={selectedBackend}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(backendId) => {
          onSelectBackend(backendId);
          onSavedStatusChange(`backend:${backendId}`);
        }}
        onSaveBackendConfig={onSaveBackendConfig}
      />
    </section>
  );
}

export type BackendConfigEditorProps = {
  backend: InferenceBackendView | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (backendId: string) => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
};

export function BackendConfigEditor({
  backend,
  savedStatus,
  saving,
  onSaved,
  onSaveBackendConfig,
}: BackendConfigEditorProps) {
  const [backendId, setBackendId] = useState("");
  const [name, setName] = useState("");
  const [providerKind, setProviderKind] = useState("openai");
  const [endpoint, setEndpoint] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiKeyEnvVar, setApiKeyEnvVar] = useState("");
  const [clearApiKey, setClearApiKey] = useState(false);
  const [models, setModels] = useState("");
  const [maxConcurrent, setMaxConcurrent] = useState("");
  const [maxQueueDepth, setMaxQueueDepth] = useState("");
  const [enabled, setEnabled] = useState(true);

  useEffect(() => {
    const base = backendFormValues(backend);
    setBackendId(base.backendId);
    setName(base.name);
    setProviderKind(base.providerKind);
    setEndpoint(base.endpoint);
    setApiKey(base.apiKey);
    setApiKeyEnvVar(base.apiKeyEnvVar);
    setClearApiKey(base.clearApiKey);
    setModels(base.models);
    setMaxConcurrent(base.maxConcurrent);
    setMaxQueueDepth(base.maxQueueDepth);
    setEnabled(base.enabled);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend?.backendId]);

  const dirty = isDirty(
    {
      backendId,
      name,
      providerKind,
      endpoint,
      apiKey,
      apiKeyEnvVar,
      clearApiKey,
      models,
      maxConcurrent,
      maxQueueDepth,
      enabled,
    },
    backendFormValues(backend),
  );

  const maxConcurrentValid = isOptionalInt(maxConcurrent, { min: 1 });
  const maxQueueDepthValid = isOptionalInt(maxQueueDepth, { min: 1 });

  async function submitBackend(event: FormEvent) {
    event.preventDefault();
    const nextId = backendId.trim();
    try {
      await onSaveBackendConfig({
        backendId: nextId,
        name,
        providerKind,
        endpoint,
        apiKey: apiKey.trim() ? apiKey : undefined,
        apiKeyEnvVar,
        clearApiKey,
        models: linesToArray(models),
        maxConcurrent: parseOptionalInt(maxConcurrent),
        maxQueueDepth: parseOptionalInt(maxQueueDepth),
        enabled,
      });
      onSaved(nextId);
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitBackend}>
      <ConfigEditorHeader
        eyebrow="Backend"
        saved={savedStatus === `backend:${backendId.trim()}`}
        title={name || backendId || "New Backend"}
        dirty={dirty}
      />
      <div className="grid-2">
        <label className="field">
          <span>Backend ID</span>
          <input
            data-testid="backend-id"
            onChange={(event) => {
              if (!backend) {
                setBackendId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(backend)}
            title={
              backend ? "Backend IDs cannot be renamed after creation." : undefined
            }
            value={backendId}
          />
        </label>
        <label className="field">
          <span>Name</span>
          <input
            data-testid="backend-name"
            onChange={(event) => setName(event.currentTarget.value)}
            value={name}
          />
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Provider kind</span>
          <select
            data-testid="backend-provider-kind"
            onChange={(event) => setProviderKind(event.currentTarget.value)}
            value={providerKind}
          >
            <option value="openai">OpenAI compatible</option>
            <option value="openrouter">OpenRouter</option>
          </select>
        </label>
        <label className="field">
          <span>Endpoint</span>
          <input
            data-testid="backend-endpoint"
            onChange={(event) => setEndpoint(event.currentTarget.value)}
            value={endpoint}
          />
        </label>
      </div>
      <label className="field">
        <span>Models</span>
        <textarea
          className="config-small-textarea"
          data-testid="backend-models"
          onChange={(event) => setModels(event.currentTarget.value)}
          value={models}
        />
      </label>
      <div className="grid-2">
        <label className="field">
          <span>API key env var</span>
          <input
            data-testid="backend-api-key-env-var"
            onChange={(event) => setApiKeyEnvVar(event.currentTarget.value)}
            value={apiKeyEnvVar}
          />
        </label>
        <label className="field">
          <span>API key</span>
          <input
            data-testid="backend-api-key"
            onChange={(event) => setApiKey(event.currentTarget.value)}
            placeholder={backend?.apiKeyConfigured ? "Configured" : ""}
            type="password"
            value={apiKey}
          />
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Max concurrent</span>
          <input
            data-testid="backend-max-concurrent"
            onChange={(event) => setMaxConcurrent(event.currentTarget.value)}
            type="number"
            value={maxConcurrent}
          />
        </label>
        <label className="field">
          <span>Max queue depth</span>
          <input
            data-testid="backend-max-queue-depth"
            onChange={(event) => setMaxQueueDepth(event.currentTarget.value)}
            type="number"
            value={maxQueueDepth}
          />
        </label>
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="backend-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
      </div>
      {backend?.apiKeyConfigured ? (
        <label className="checkbox">
          <input
            checked={clearApiKey}
            data-testid="backend-clear-api-key"
            onChange={(event) => setClearApiKey(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Clear stored API key</span>
        </label>
      ) : null}
      <div className="config-actions">
        <button
          className="primary-button"
          data-testid="backend-save"
          disabled={
            saving ||
            !backendId.trim() ||
            !name.trim() ||
            !endpoint.trim() ||
            !linesToArray(models).length ||
            !maxConcurrentValid ||
            !maxQueueDepthValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Backend"}
        </button>
      </div>
    </form>
  );
}

/** View→form hydration, shared by the reset effect and dirty comparison. */
function backendFormValues(backend: InferenceBackendView | null) {
  return {
    backendId: backend?.backendId ?? "",
    name: backend?.name ?? backend?.backendId ?? "",
    providerKind:
      backend?.providerKind === "OpenRouter" || backend?.providerKind === "openrouter"
        ? "openrouter"
        : "openai",
    endpoint: backend?.endpoint ?? "",
    apiKey: "",
    apiKeyEnvVar: backend?.apiKeyEnvVar ?? "",
    clearApiKey: false,
    models: (backend?.models ?? []).join("\n"),
    maxConcurrent: backend?.maxConcurrent != null ? String(backend.maxConcurrent) : "",
    maxQueueDepth: backend?.maxQueueDepth != null ? String(backend.maxQueueDepth) : "",
    enabled: backend?.enabled ?? true,
  };
}
