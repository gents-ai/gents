import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  BackendDeleteRequest,
  BackendSaveRequest,
  ConfigComponentsPatchRequest,
  BackendAuth,
  BackendProviderKind,
  InferenceBackend,
  DeploymentView,
  InferenceBackendView,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { isOptionalInt, parseOptionalInt } from "./formUtils";

export type BackendConfigPanelProps = {
  deployment: DeploymentView;
  selectedBackendId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectBackend: (backendId: string) => void;
  onCreateBackend: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onPatchConfigComponents: (request: ConfigComponentsPatchRequest) => Promise<unknown>;
  onDeleteBackendConfig: (request: BackendDeleteRequest) => Promise<unknown>;
  onDeletedBackend: () => void;
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
  onPatchConfigComponents,
  onDeleteBackendConfig,
  onDeletedBackend,
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
        agentDid={deployment.agentDid}
        backend={selectedBackend}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(backendId) => {
          onSelectBackend(backendId);
          onSavedStatusChange(`backend:${backendId}`);
        }}
        onSaveBackendConfig={onSaveBackendConfig}
        onPatchConfigComponents={onPatchConfigComponents}
        onDeleteBackendConfig={onDeleteBackendConfig}
        onDeleted={() => {
          onDeletedBackend();
        }}
      />
    </section>
  );
}

export type BackendConfigEditorProps = {
  agentDid: string;
  backend: InferenceBackendView | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (backendId: string) => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onPatchConfigComponents: (request: ConfigComponentsPatchRequest) => Promise<unknown>;
  onDeleteBackendConfig: (request: BackendDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
};

export function BackendConfigEditor({
  agentDid,
  backend,
  savedStatus,
  saving,
  onSaved,
  onSaveBackendConfig,
  onPatchConfigComponents,
  onDeleteBackendConfig,
  onDeleted,
}: BackendConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteBackend() {
    setConfirmingDelete(false);
    if (!backend) {
      return;
    }
    try {
      await onDeleteBackendConfig({ backendId: backend.backendId, agentDid });
      onDeleted();
    } catch {}
  }
  const [backendId, setBackendId] = useState("");
  const [name, setName] = useState("");
  const [providerKind, setProviderKind] =
    useState<BackendProviderKind>("OpenAiCompatible");
  const [endpoint, setEndpoint] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiKeyEnvVar, setApiKeyEnvVar] = useState("");
  const [clearApiKey, setClearApiKey] = useState(false);
  const [maxConcurrent, setMaxConcurrent] = useState("");
  const [maxQueueDepth, setMaxQueueDepth] = useState("");
  const [enabled, setEnabled] = useState(true);

  const [base, setBase] = useState(() => backendFormValues(backend));
  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const base = backendFormValues(backend);
    setBase(base);
    setBackendId(base.backendId);
    setName(base.name);
    setProviderKind(base.providerKind);
    setEndpoint(base.endpoint);
    setApiKey(base.apiKey);
    setApiKeyEnvVar(base.apiKeyEnvVar);
    setClearApiKey(base.clearApiKey);
    setMaxConcurrent(base.maxConcurrent);
    setMaxQueueDepth(base.maxQueueDepth);
    setEnabled(base.enabled);
    setSaveError(null);
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
      maxConcurrent,
      maxQueueDepth,
      enabled,
    },
    backendFormValues(backend),
  );

  const maxConcurrentValid = isOptionalInt(maxConcurrent, { min: 1 });
  const maxQueueDepthValid = isOptionalInt(maxQueueDepth, { min: 0 });

  async function submitBackend(event: FormEvent) {
    event.preventDefault();
    const nextId = backend?.backendId ?? backendId.trim();
    try {
      if (apiKey.trim() && apiKeyEnvVar.trim()) {
        throw new Error("Choose an API key or an environment variable, not both.");
      }
      let auth: BackendAuth | undefined;
      if (apiKey.trim()) auth = { kind: "api_key", key: apiKey };
      else if (
        apiKeyEnvVar !== base.apiKeyEnvVar ||
        (!backend && apiKeyEnvVar.trim())
      ) {
        auth = apiKeyEnvVar.trim()
          ? { kind: "environment", variable: apiKeyEnvVar }
          : { kind: "unauthenticated" };
      } else if (clearApiKey) auth = { kind: "unauthenticated" };

      if (backend) {
        const changes: Partial<Omit<InferenceBackend, "agent_did" | "backend_id">> = {};
        if (name !== base.name) changes.name = name;
        if (providerKind !== base.providerKind) changes.provider_kind = providerKind;
        if (endpoint !== base.endpoint) changes.endpoint = endpoint;
        if (maxConcurrent !== base.maxConcurrent)
          changes.max_concurrent = parseOptionalInt(maxConcurrent);
        if (maxQueueDepth !== base.maxQueueDepth)
          changes.max_queue_depth = parseOptionalInt(maxQueueDepth);
        if (enabled !== base.enabled) changes.enabled = enabled;
        if (auth) changes.auth = auth;
        await onPatchConfigComponents({
          agentDid,
          patches: [{ collection: "InferenceBackend", id: nextId, changes }],
        });
      } else {
        await onSaveBackendConfig({
          document: {
            agent_did: agentDid,
            backend_id: nextId,
            name,
            provider_kind: providerKind,
            endpoint,
            auth: auth ?? { kind: "unauthenticated" },
            max_concurrent: parseOptionalInt(maxConcurrent),
            max_queue_depth: parseOptionalInt(maxQueueDepth),
            enabled,
          },
        });
      }
      setApiKey("");
      setClearApiKey(false);
      setBase({
        backendId: nextId,
        name,
        providerKind,
        endpoint,
        apiKey: "",
        apiKeyEnvVar,
        clearApiKey: false,
        maxConcurrent,
        maxQueueDepth,
        enabled,
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
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
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
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
            onChange={(event) =>
              setProviderKind(event.currentTarget.value as BackendProviderKind)
            }
            value={providerKind}
          >
            <option value="OpenAiCompatible">OpenAI compatible</option>
            <option value="OpenRouter">OpenRouter</option>
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
      {backend ? (
        <label className="field">
          <span>Advertised models</span>
          <textarea
            className="config-small-textarea"
            data-testid="backend-models"
            readOnly
            value={backend.models.join("\n")}
          />
          <span>Select the model on an inference profile.</span>
        </label>
      ) : null}
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
          <FieldHint show={!maxConcurrentValid}>Whole number of 1 or more</FieldHint>
        </label>
        <label className="field">
          <span>Max queue depth</span>
          <input
            data-testid="backend-max-queue-depth"
            onChange={(event) => setMaxQueueDepth(event.currentTarget.value)}
            type="number"
            value={maxQueueDepth}
          />
          <FieldHint show={!maxQueueDepthValid}>Whole number of 0 or more</FieldHint>
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
        {backend ? (
          <button
            className="ghost-button danger-button"
            data-testid="backend-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Backend
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete backend"
          message={`Delete backend "${backend?.backendId ?? ""}"? Behaviors still pointing at it will block the delete.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            void deleteBackend();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="backend-save"
          disabled={
            saving ||
            !backendId.trim() ||
            !name.trim() ||
            !endpoint.trim() ||
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

function backendFormValues(backend: InferenceBackendView | null) {
  return {
    backendId: backend?.backendId ?? "",
    name: backend?.name ?? backend?.backendId ?? "",
    providerKind: (backend?.providerKind ?? "OpenAiCompatible") as BackendProviderKind,
    endpoint: backend?.endpoint ?? "",
    apiKey: "",
    apiKeyEnvVar: backend?.apiKeyEnvVar ?? "",
    clearApiKey: false,
    maxConcurrent: backend?.maxConcurrent != null ? String(backend.maxConcurrent) : "",
    maxQueueDepth: backend?.maxQueueDepth != null ? String(backend.maxQueueDepth) : "",
    enabled: backend?.enabled ?? true,
  };
}
