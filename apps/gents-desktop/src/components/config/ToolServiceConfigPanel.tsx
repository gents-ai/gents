import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ToolServiceRegistry,
  ToolServiceDeleteRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
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
        (service) => service.service_id === selectedToolServiceId,
      ) ?? null,
    [deployment.toolServiceRegistries, selectedToolServiceId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Tools"
        items={deployment.toolServiceRegistries.map((service) => ({
          id: service.service_id,
          title: service.display_name ?? service.service_id,
          meta: [
            service.enabled === false ? "disabled" : "service",
            service.hostname ?? service.tailscale_ip ?? service.lan_ip ?? null,
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
        agentDid={deployment.agentDid}
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
  agentDid: string;
  toolService: ToolServiceRegistry | null;
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
  agentDid,
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
      await onDeleteToolServiceConfig({ serviceId: toolService.service_id, agentDid });
      onDeleted();
    } catch {}
  }
  const [serviceId, setServiceId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [hostname, setHostname] = useState("");
  const [tailscaleIp, setTailscaleIp] = useState("");
  const [lanIp, setLanIp] = useState("");
  const [mcpPort, setMcpPort] = useState("");
  const [mcpPath, setMcpPath] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [sendAgentDid, setSendAgentDid] = useState(false);
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
    setEnabled(b.enabled);
    setSendAgentDid(b.sendAgentDid);
    setTestResult(null);
    setTestError(null);
    setSaveError(null);
  }, [toolService?.service_id]);

  const mcpPortValid = isOptionalInt(mcpPort, { min: 1, max: 65535 });
  const serviceAddressPresent = Boolean(
    hostname.trim() || tailscaleIp.trim() || lanIp.trim(),
  );

  function currentTestRequest(): ToolServiceTestRequest {
    return {
      serviceId: toolService?.service_id ?? serviceId.trim(),
      hostname: optionalString(hostname),
      tailscaleIp: optionalString(tailscaleIp),
      lanIp: optionalString(lanIp),
      mcpPort: parseOptionalInt(mcpPort),
      mcpPath: optionalString(mcpPath),
    };
  }

  async function submitToolService(event: FormEvent) {
    event.preventDefault();
    const nextId = toolService?.service_id ?? serviceId.trim();
    try {
      await onSaveToolServiceConfig({
        document: {
          ...toolService,
          agent_did: agentDid,
          service_id: nextId,
          display_name: displayName || null,
          description: optionalString(description),
          hostname: optionalString(hostname),
          tailscale_ip: optionalString(tailscaleIp),
          lan_ip: optionalString(lanIp),
          mcp_port: parseOptionalInt(mcpPort),
          mcp_path: optionalString(mcpPath),
          enabled,
          send_agent_did: sendAgentDid,
        },
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
            enabled,
            sendAgentDid,
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
        <label className="checkbox">
          <input
            data-testid="tool-service-enabled"
            type="checkbox"
            checked={enabled}
            onChange={(event) => setEnabled(event.currentTarget.checked)}
          />
          <span>Enabled</span>
        </label>
        <label className="checkbox">
          <input
            data-testid="tool-service-send-agent-did"
            type="checkbox"
            checked={sendAgentDid}
            onChange={(event) => setSendAgentDid(event.currentTarget.checked)}
          />
          <span>Send agent DID to the service</span>
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
            !mcpPortValid
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
          message={`Delete tool service "${toolService?.service_id ?? ""}"? Selections still allowing it will block the delete.`}
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
            !mcpPort.trim() ||
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

function toolServiceFormValues(toolService: ToolServiceRegistry | null) {
  return {
    serviceId: toolService?.service_id ?? "",
    displayName: toolService?.display_name ?? toolService?.service_id ?? "",
    description: toolService?.description ?? "",
    hostname: toolService?.hostname ?? "",
    tailscaleIp: toolService?.tailscale_ip ?? "",
    lanIp: toolService?.lan_ip ?? "",
    mcpPort: toolService?.mcp_port != null ? String(toolService.mcp_port) : "",
    mcpPath: toolService?.mcp_path ?? "",
    enabled: toolService?.enabled ?? true,
    sendAgentDid: toolService?.send_agent_did ?? false,
  };
}
