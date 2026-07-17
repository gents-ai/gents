import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ToolServiceRegistryView,
  ToolServiceDeleteRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "../../lib/types";
import { ConfirmDialog } from "../ConfirmDialog";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { isOptionalInt, optionalString, parseOptionalInt } from "./formUtils";

export type ToolServiceConfigPanelProps = {
  deployment: DeploymentView;
  selectedToolServiceId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectToolService: (serviceId: string) => void;
  onCreateToolService: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveToolServiceConfig: (request: ToolServiceSaveRequest) => Promise<unknown>;
  onDeleteToolServiceConfig: (request: ToolServiceDeleteRequest) => Promise<unknown>;
  onDeletedToolService: () => void;
  onTestToolService: (
    request: ToolServiceTestRequest,
  ) => Promise<ToolServiceTestResult>;
};

export function ToolServiceConfigPanel({
  deployment,
  selectedToolServiceId,
  saving,
  savedStatus,
  onSelectToolService,
  onCreateToolService,
  onSavedStatusChange,
  onSaveToolServiceConfig,
  onDeleteToolServiceConfig,
  onDeletedToolService,
  onTestToolService,
}: ToolServiceConfigPanelProps) {
  const selectedToolService = useMemo(
    () =>
      deployment.toolServiceRegistries.find(
        (service) => service.serviceId === selectedToolServiceId,
      ) ?? null,
    [deployment.toolServiceRegistries, selectedToolServiceId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Tools"
        items={deployment.toolServiceRegistries.map((service) => ({
          id: service.serviceId,
          title: service.displayName ?? service.serviceId,
          meta: [
            service.status ?? "service",
            service.hostname ?? service.tailscaleIp ?? service.lanIp ?? null,
          ]
            .filter(Boolean)
            .join(" / "),
        }))}
        selectedId={selectedToolServiceId}
        testPrefix="tool-service"
        title="HTTP MCP Services"
        onCreate={onCreateToolService}
        onSelect={onSelectToolService}
      />

      <ToolServiceConfigEditor
        savedStatus={savedStatus}
        saving={saving}
        toolService={selectedToolService}
        onSaved={(serviceId) => {
          onSelectToolService(serviceId);
          onSavedStatusChange(`tool-service:${serviceId}`);
        }}
        onSaveToolServiceConfig={onSaveToolServiceConfig}
        onDeleteToolServiceConfig={onDeleteToolServiceConfig}
        onDeleted={() => {
          onDeletedToolService();
        }}
        onTestToolService={onTestToolService}
      />
    </section>
  );
}

export type ToolServiceConfigEditorProps = {
  toolService: ToolServiceRegistryView | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (serviceId: string) => void;
  onSaveToolServiceConfig: (request: ToolServiceSaveRequest) => Promise<unknown>;
  onDeleteToolServiceConfig: (request: ToolServiceDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
  onTestToolService: (
    request: ToolServiceTestRequest,
  ) => Promise<ToolServiceTestResult>;
};

export function ToolServiceConfigEditor({
  toolService,
  savedStatus,
  saving,
  onSaved,
  onSaveToolServiceConfig,
  onDeleteToolServiceConfig,
  onDeleted,
  onTestToolService,
}: ToolServiceConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteToolService() {
    setConfirmingDelete(false);
    if (!toolService) {
      return;
    }
    try {
      await onDeleteToolServiceConfig({ serviceId: toolService.serviceId });
      onDeleted();
    } catch {
      // Surfaced by the shell error banner; the editor stays put.
    }
  }
  const [serviceId, setServiceId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [hostname, setHostname] = useState("");
  const [tailscaleIp, setTailscaleIp] = useState("");
  const [lanIp, setLanIp] = useState("");
  const [mcpPort, setMcpPort] = useState("");
  const [mcpPath, setMcpPath] = useState("/mcp");
  const [status, setStatus] = useState("online");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<ToolServiceTestResult | null>(null);
  const [testError, setTestError] = useState<string | null>(null);

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const b = toolServiceFormValues(toolService);
    setServiceId(b.serviceId);
    setDisplayName(b.displayName);
    setDescription(b.description);
    setHostname(b.hostname);
    setTailscaleIp(b.tailscaleIp);
    setLanIp(b.lanIp);
    setMcpPort(b.mcpPort);
    setMcpPath(b.mcpPath);
    setStatus(b.status);
    setTestResult(null);
    setTestError(null);
    setSaveError(null);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
  }, [toolService?.serviceId]);

  const mcpPortValid = isOptionalInt(mcpPort, { min: 1, max: 65535 });
  const serviceAddressPresent = Boolean(
    hostname.trim() || tailscaleIp.trim() || lanIp.trim(),
  );

  function currentTestRequest(): ToolServiceTestRequest {
    return {
      serviceId: serviceId.trim(),
      hostname: optionalString(hostname),
      tailscaleIp: optionalString(tailscaleIp),
      lanIp: optionalString(lanIp),
      mcpPort: parseOptionalInt(mcpPort),
      mcpPath: optionalString(mcpPath) || "/mcp",
    };
  }

  async function submitToolService(event: FormEvent) {
    event.preventDefault();
    const nextId = serviceId.trim();
    try {
      await onSaveToolServiceConfig({
        serviceId: nextId,
        displayName,
        description: optionalString(description),
        hostname: optionalString(hostname),
        tailscaleIp: optionalString(tailscaleIp),
        lanIp: optionalString(lanIp),
        mcpPort: parseOptionalInt(mcpPort),
        mcpPath: optionalString(mcpPath) || "/mcp",
        status: optionalString(status) || "online",
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  async function testToolService() {
    setTesting(true);
    setTestResult(null);
    setTestError(null);
    try {
      const result = await onTestToolService(currentTestRequest());
      setTestResult(result);
    } catch (error) {
      setTestError(String(error));
    } finally {
      setTesting(false);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitToolService}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            serviceId,
            displayName,
            description,
            hostname,
            tailscaleIp,
            lanIp,
            mcpPort,
            mcpPath,
            status,
          },
          toolServiceFormValues(toolService),
        )}
        eyebrow="HTTP MCP Service"
        saved={savedStatus === `tool-service:${serviceId.trim()}`}
        title={displayName || serviceId || "New Service"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Service ID</span>
          <input
            data-testid="tool-service-id"
            onChange={(event) => {
              if (!toolService) {
                setServiceId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(toolService)}
            title={
              toolService
                ? "Tool service IDs cannot be renamed after creation."
                : undefined
            }
            value={serviceId}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="tool-service-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>
      <label className="field">
        <span>Description</span>
        <textarea
          className="config-small-textarea"
          data-testid="tool-service-description"
          onChange={(event) => setDescription(event.currentTarget.value)}
          value={description}
        />
      </label>
      <div className="grid-3">
        <label className="field">
          <span>Hostname</span>
          <input
            data-testid="tool-service-hostname"
            onChange={(event) => setHostname(event.currentTarget.value)}
            value={hostname}
          />
        </label>
        <label className="field">
          <span>Tailscale IP</span>
          <input
            data-testid="tool-service-tailscale-ip"
            onChange={(event) => setTailscaleIp(event.currentTarget.value)}
            value={tailscaleIp}
          />
        </label>
        <label className="field">
          <span>LAN IP</span>
          <input
            data-testid="tool-service-lan-ip"
            onChange={(event) => setLanIp(event.currentTarget.value)}
            value={lanIp}
          />
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>MCP port</span>
          <input
            data-testid="tool-service-mcp-port"
            onChange={(event) => setMcpPort(event.currentTarget.value)}
            type="number"
            value={mcpPort}
          />
          <FieldHint show={!mcpPortValid}>Port between 1 and 65535</FieldHint>
        </label>
        <label className="field">
          <span>MCP path</span>
          <input
            data-testid="tool-service-mcp-path"
            onChange={(event) => setMcpPath(event.currentTarget.value)}
            value={mcpPath}
          />
        </label>
        <label className="field">
          <span>Status</span>
          <select
            data-testid="tool-service-status"
            onChange={(event) => setStatus(event.currentTarget.value)}
            value={status}
          >
            <option value="online">Online</option>
            <option value="offline">Offline</option>
            <option value="disabled">Disabled</option>
          </select>
        </label>
      </div>
      <div className="config-actions">
        <button
          className="ghost-button"
          data-testid="tool-service-test"
          disabled={
            testing ||
            !serviceId.trim() ||
            !serviceAddressPresent ||
            !mcpPort.trim() ||
            !mcpPortValid ||
            !mcpPath.trim()
          }
          onClick={() => void testToolService()}
          type="button"
        >
          {testing ? "Testing..." : "Test Service"}
        </button>
        {toolService ? (
          <button
            className="ghost-button danger-button"
            data-testid="tool-service-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Service
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete tool service"
          message={`Delete tool service "${toolService?.serviceId ?? ""}"? Selections still allowing it will block the delete.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            void deleteToolService();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="tool-service-save"
          disabled={
            saving ||
            !serviceId.trim() ||
            !displayName.trim() ||
            !mcpPath.trim() ||
            !mcpPortValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Service"}
        </button>
      </div>
      {testResult ? (
        <div className="config-result" data-testid="tool-service-test-result">
          <div className="facts">
            <div>
              <dt>Endpoint</dt>
              <dd className="mono">{testResult.endpoint}</dd>
            </div>
            <div>
              <dt>Tools</dt>
              <dd>{testResult.toolCount}</dd>
            </div>
          </div>
          {testResult.tools.length ? (
            <div className="run-history">
              {testResult.tools.slice(0, 8).map((tool) => (
                <div className="run-history-row" key={tool.name}>
                  <span className="mono">{tool.name}</span>
                  <span className="muted">{tool.description ?? ""}</span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
      {testError ? (
        <div
          className="config-result config-result-error"
          data-testid="tool-service-test-error"
        >
          {testError}
        </div>
      ) : null}
    </form>
  );
}

/** View→form hydration, shared by the reset effect and dirty comparison. */
function toolServiceFormValues(toolService: ToolServiceRegistryView | null) {
  return {
    serviceId: toolService?.serviceId ?? "",
    displayName: toolService?.displayName ?? toolService?.serviceId ?? "",
    description: toolService?.description ?? "",
    hostname: toolService?.hostname ?? "",
    tailscaleIp: toolService?.tailscaleIp ?? "",
    lanIp: toolService?.lanIp ?? "",
    mcpPort: toolService?.mcpPort != null ? String(toolService.mcpPort) : "",
    mcpPath: toolService?.mcpPath ?? "/mcp",
    status: toolService?.status ?? "online",
  };
}
