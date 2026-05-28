import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ToolServiceRegistryView,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import {
  ignoreHandledActionError,
  isOptionalInt,
  optionalString,
  parseOptionalInt,
} from "./formUtils";

export type ToolServiceConfigPanelProps = {
  deployment: DeploymentView;
  selectedToolServiceId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectToolService: (serviceId: string) => void;
  onCreateToolService: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveToolServiceConfig: (request: ToolServiceSaveRequest) => Promise<unknown>;
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
  onTestToolService,
}: ToolServiceConfigEditorProps) {
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

  useEffect(() => {
    setServiceId(toolService?.serviceId ?? "");
    setDisplayName(toolService?.displayName ?? toolService?.serviceId ?? "");
    setDescription(toolService?.description ?? "");
    setHostname(toolService?.hostname ?? "");
    setTailscaleIp(toolService?.tailscaleIp ?? "");
    setLanIp(toolService?.lanIp ?? "");
    setMcpPort(toolService?.mcpPort != null ? String(toolService.mcpPort) : "");
    setMcpPath(toolService?.mcpPath ?? "/mcp");
    setStatus(toolService?.status ?? "online");
    setTestResult(null);
    setTestError(null);
  }, [toolService]);

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
    } catch (error) {
      ignoreHandledActionError(error);
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
        eyebrow="HTTP MCP Service"
        saved={savedStatus === `tool-service:${serviceId.trim()}`}
        title={displayName || serviceId || "New Service"}
      />
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
