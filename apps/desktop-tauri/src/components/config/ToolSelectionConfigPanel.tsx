import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ToolSelectionSaveRequest,
  ToolSelectionView,
  ToolServiceRegistryView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import {
  ignoreHandledActionError,
  isOptionalInt,
  linesToArray,
  parseOptionalInt,
} from "./formUtils";

const COMMAND_POLICY_OPTIONS = [
  { value: "", label: "Default for bash mode" },
  { value: "read_only", label: "Read only" },
  { value: "workspace_write", label: "Workspace write" },
  { value: "unrestricted", label: "Unrestricted" },
] as const;

const COMMAND_NETWORK_OPTIONS = [
  { value: "", label: "Inherit" },
  { value: "disabled", label: "Disabled" },
  { value: "enabled", label: "Enabled" },
] as const;

export type ToolSelectionConfigPanelProps = {
  deployment: DeploymentView;
  selectedToolSelectionId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectToolSelection: (selectionId: string) => void;
  onCreateToolSelection: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveToolSelectionConfig: (request: ToolSelectionSaveRequest) => Promise<unknown>;
  toolCeiling?: string | null;
  toolRoot?: string | null;
};

export function ToolSelectionConfigPanel({
  deployment,
  selectedToolSelectionId,
  saving,
  savedStatus,
  onSelectToolSelection,
  onCreateToolSelection,
  onSavedStatusChange,
  onSaveToolSelectionConfig,
  toolCeiling,
  toolRoot,
}: ToolSelectionConfigPanelProps) {
  const selectedToolSelection = useMemo(
    () =>
      deployment.toolSelections.find(
        (selection) => selection.selectionId === selectedToolSelectionId,
      ) ?? null,
    [deployment.toolSelections, selectedToolSelectionId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Tools"
        items={deployment.toolSelections.map((selection) => ({
          id: selection.selectionId,
          title: selection.displayName ?? selection.selectionId,
          meta:
            [
              selection.enableFileTools ? "files" : null,
              selection.enableBash ? "bash" : null,
              selection.enableMetaTools ? "meta" : null,
              selection.subagentTargets?.length ? "subagents" : null,
            ]
              .filter(Boolean)
              .join(" / ") || "tool selection",
        }))}
        selectedId={selectedToolSelectionId}
        testPrefix="tool-selection"
        title="Tool Selections"
        onCreate={onCreateToolSelection}
        onSelect={onSelectToolSelection}
      />

      <ToolSelectionConfigEditor
        agentDid={deployment.agentDid}
        savedStatus={savedStatus}
        saving={saving}
        toolCeiling={toolCeiling}
        toolRoot={toolRoot}
        toolServiceRegistries={deployment.toolServiceRegistries}
        toolSelection={selectedToolSelection}
        onSaved={(selectionId) => {
          onSelectToolSelection(selectionId);
          onSavedStatusChange(`tool:${selectionId}`);
        }}
        onSaveToolSelectionConfig={onSaveToolSelectionConfig}
      />
    </section>
  );
}

export type ToolSelectionConfigEditorProps = {
  agentDid: string;
  toolSelection: ToolSelectionView | null;
  toolServiceRegistries: ToolServiceRegistryView[];
  toolCeiling?: string | null;
  toolRoot?: string | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (selectionId: string) => void;
  onSaveToolSelectionConfig: (request: ToolSelectionSaveRequest) => Promise<unknown>;
};

export function ToolSelectionConfigEditor({
  agentDid,
  toolSelection,
  toolServiceRegistries,
  toolCeiling,
  toolRoot,
  savedStatus,
  saving,
  onSaved,
  onSaveToolSelectionConfig,
}: ToolSelectionConfigEditorProps) {
  const [selectionId, setSelectionId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [enableFileTools, setEnableFileTools] = useState(false);
  const [fileToolsMode, setFileToolsMode] = useState("ReadOnly");
  const [fileToolRoot, setFileToolRoot] = useState("");
  const [enableBash, setEnableBash] = useState(false);
  const [bashMode, setBashMode] = useState("ReadOnly");
  const [commandExecutionPolicy, setCommandExecutionPolicy] = useState("");
  const [commandAllowedArgvPrefixes, setCommandAllowedArgvPrefixes] = useState("");
  const [commandForbiddenArgvPrefixes, setCommandForbiddenArgvPrefixes] = useState("");
  const [commandNetworkMode, setCommandNetworkMode] = useState("");
  const [cliToolNames, setCliToolNames] = useState("");
  const [enableMetaTools, setEnableMetaTools] = useState(false);
  const [allowedMcpServiceIds, setAllowedMcpServiceIds] = useState("");
  const [delegateTo, setDelegateTo] = useState("");
  const [backgroundableToolNames, setBackgroundableToolNames] = useState("");
  const [subagentTargets, setSubagentTargets] = useState("");
  const [subagentSpawnEnabled, setSubagentSpawnEnabled] = useState(false);
  const [subagentSteeringEnabled, setSubagentSteeringEnabled] = useState(false);
  const [subagentBackgroundEnabled, setSubagentBackgroundEnabled] = useState(false);
  const [crossDeploymentSpawnTimeoutSeconds, setCrossDeploymentSpawnTimeoutSeconds] =
    useState("");
  const ceiling = normalizeToolCeiling(toolCeiling);
  const fileToolsDisabledByCeiling = ceiling === "MetaOnly";
  const writeToolsDisabledByCeiling = ceiling !== "Readwrite";
  const crossDeploymentSpawnTimeoutValid = isOptionalInt(
    crossDeploymentSpawnTimeoutSeconds,
    { min: 1 },
  );
  const toolServiceIdKey = useMemo(
    () =>
      toolServiceRegistries
        .map((service) => service.serviceId)
        .sort()
        .join("\n"),
    [toolServiceRegistries],
  );

  useEffect(() => {
    setSelectionId(toolSelection?.selectionId ?? "");
    setDisplayName(toolSelection?.displayName ?? toolSelection?.selectionId ?? "");
    setEnableFileTools(toolSelection?.enableFileTools ?? false);
    setFileToolsMode(toolSelection?.fileToolsMode ?? "ReadOnly");
    setFileToolRoot(toolSelection?.fileToolRoot ?? "");
    setEnableBash(toolSelection?.enableBash ?? false);
    setBashMode(
      toolSelection?.bashMode === "ReadWrite"
        ? "Unrestricted"
        : (toolSelection?.bashMode ?? "ReadOnly"),
    );
    setCommandExecutionPolicy(
      normalizeCommandExecutionPolicy(toolSelection?.commandExecutionPolicy),
    );
    setCommandAllowedArgvPrefixes(
      (toolSelection?.commandAllowedArgvPrefixes ?? []).join("\n"),
    );
    setCommandForbiddenArgvPrefixes(
      (toolSelection?.commandForbiddenArgvPrefixes ?? []).join("\n"),
    );
    setCommandNetworkMode(normalizeCommandNetworkMode(toolSelection?.commandNetworkMode));
    setCliToolNames((toolSelection?.cliToolNames ?? []).join("\n"));
    setEnableMetaTools(toolSelection?.enableMetaTools ?? false);
    const knownServiceIds = new Set(toolServiceIdKey.split("\n").filter(Boolean));
    const existingAllowedServiceIds = toolSelection?.allowedMcpServiceIds ?? [];
    const existingDelegateTo = toolSelection?.delegateTo ?? [];
    const legacyServiceDelegates =
      existingAllowedServiceIds.length === 0
        ? existingDelegateTo.filter((value) => knownServiceIds.has(value))
        : [];
    setAllowedMcpServiceIds(
      (existingAllowedServiceIds.length > 0
        ? existingAllowedServiceIds
        : legacyServiceDelegates
      ).join("\n"),
    );
    setDelegateTo(
      existingDelegateTo.filter((value) => !knownServiceIds.has(value)).join("\n"),
    );
    setBackgroundableToolNames(
      (toolSelection?.backgroundableToolNames ?? []).join("\n"),
    );
    setSubagentTargets((toolSelection?.subagentTargets ?? []).join("\n"));
    setSubagentSpawnEnabled(toolSelection?.subagentSpawnEnabled ?? false);
    setSubagentSteeringEnabled(toolSelection?.subagentSteeringEnabled ?? false);
    setSubagentBackgroundEnabled(toolSelection?.subagentBackgroundEnabled ?? false);
    setCrossDeploymentSpawnTimeoutSeconds(
      toolSelection?.crossDeploymentSpawnTimeoutSeconds != null
        ? String(toolSelection.crossDeploymentSpawnTimeoutSeconds)
        : "",
    );
  }, [toolSelection, toolServiceIdKey]);

  function toggleAllowedMcpService(serviceId: string, checked: boolean) {
    const values = new Set(linesToArray(allowedMcpServiceIds));
    if (checked) {
      values.add(serviceId);
    } else {
      values.delete(serviceId);
    }
    setAllowedMcpServiceIds(Array.from(values).sort().join("\n"));
  }

  async function submitToolSelection(event: FormEvent) {
    event.preventDefault();
    const nextId = selectionId.trim();
    const effectiveEnableFileTools = !fileToolsDisabledByCeiling && enableFileTools;
    const effectiveEnableBash = !fileToolsDisabledByCeiling && enableBash;
    const effectiveFileToolsMode =
      writeToolsDisabledByCeiling && fileToolsMode === "ReadWrite"
        ? "ReadOnly"
        : fileToolsMode;
    const effectiveBashMode =
      writeToolsDisabledByCeiling && bashMode === "Unrestricted"
        ? "ReadOnly"
        : bashMode;
    try {
      await onSaveToolSelectionConfig({
        agentDid,
        selectionId: nextId,
        displayName,
        enableFileTools: effectiveEnableFileTools,
        fileToolsMode: effectiveFileToolsMode,
        fileToolRoot,
        enableBash: effectiveEnableBash,
        bashMode: effectiveBashMode,
        commandExecutionPolicy,
        commandAllowedArgvPrefixes: linesToArray(commandAllowedArgvPrefixes),
        commandForbiddenArgvPrefixes: linesToArray(commandForbiddenArgvPrefixes),
        commandNetworkMode,
        cliToolNames: linesToArray(cliToolNames),
        enableMetaTools,
        allowedMcpServiceIds: linesToArray(allowedMcpServiceIds),
        delegateTo: linesToArray(delegateTo),
        backgroundableToolNames: linesToArray(backgroundableToolNames),
        subagentTargets: linesToArray(subagentTargets),
        subagentSpawnEnabled,
        subagentSteeringEnabled,
        subagentBackgroundEnabled,
        crossDeploymentSpawnTimeoutSeconds: parseOptionalInt(
          crossDeploymentSpawnTimeoutSeconds,
        ),
      });
      onSaved(nextId);
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitToolSelection}>
      <ConfigEditorHeader
        eyebrow="Tool Selection"
        saved={savedStatus === `tool:${selectionId.trim()}`}
        title={displayName || selectionId || "New Tool Selection"}
      />
      <div className="facts">
        <div>
          <dt>Server ceiling</dt>
          <dd>{displayToolCeiling(ceiling)}</dd>
        </div>
        <div>
          <dt>Server tool root</dt>
          <dd className="mono">{toolRoot || "not configured"}</dd>
        </div>
        <div>
          <dt>Agent DID</dt>
          <dd className="mono">{agentDid}</dd>
        </div>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Selection ID</span>
          <input
            data-testid="tool-selection-id"
            onChange={(event) => {
              if (!toolSelection) {
                setSelectionId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(toolSelection)}
            title={
              toolSelection
                ? "Tool selection IDs cannot be renamed after creation."
                : undefined
            }
            value={selectionId}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="tool-selection-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>
      <div className="grid-2">
        <label className="checkbox">
          <input
            checked={enableFileTools}
            data-testid="tool-enable-file-tools"
            disabled={fileToolsDisabledByCeiling}
            onChange={(event) => setEnableFileTools(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>File tools</span>
        </label>
        <label className="field">
          <span>File tools mode</span>
          <select
            data-testid="tool-file-tools-mode"
            disabled={!enableFileTools || fileToolsDisabledByCeiling}
            onChange={(event) => setFileToolsMode(event.currentTarget.value)}
            value={fileToolsMode}
          >
            <option value="ReadOnly">Read only</option>
            <option disabled={writeToolsDisabledByCeiling} value="ReadWrite">
              Read write
            </option>
          </select>
        </label>
      </div>
      <label className="field">
        <span>File tool root</span>
        <input
          data-testid="tool-file-tool-root"
          disabled={!enableFileTools || fileToolsDisabledByCeiling}
          onChange={(event) => setFileToolRoot(event.currentTarget.value)}
          value={fileToolRoot}
        />
      </label>
      <div className="grid-2">
        <label className="checkbox">
          <input
            checked={enableBash}
            data-testid="tool-enable-bash"
            disabled={fileToolsDisabledByCeiling}
            onChange={(event) => setEnableBash(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Bash</span>
        </label>
        <label className="field">
          <span>Bash mode</span>
          <select
            data-testid="tool-bash-mode"
            disabled={!enableBash || fileToolsDisabledByCeiling}
            onChange={(event) => setBashMode(event.currentTarget.value)}
            value={bashMode}
          >
            <option value="ReadOnly">Read only</option>
            <option disabled={writeToolsDisabledByCeiling} value="Unrestricted">
              Unrestricted
            </option>
          </select>
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Command policy</span>
          <select
            data-testid="tool-command-execution-policy"
            onChange={(event) => setCommandExecutionPolicy(event.currentTarget.value)}
            value={commandExecutionPolicy}
          >
            {COMMAND_POLICY_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Command network mode</span>
          <select
            data-testid="tool-command-network-mode"
            onChange={(event) => setCommandNetworkMode(event.currentTarget.value)}
            value={commandNetworkMode}
          >
            {COMMAND_NETWORK_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Allowed argv prefixes</span>
          <textarea
            className="config-small-textarea"
            data-testid="tool-command-allowed-argv-prefixes"
            onChange={(event) =>
              setCommandAllowedArgvPrefixes(event.currentTarget.value)
            }
            value={commandAllowedArgvPrefixes}
          />
        </label>
        <label className="field">
          <span>Forbidden argv prefixes</span>
          <textarea
            className="config-small-textarea"
            data-testid="tool-command-forbidden-argv-prefixes"
            onChange={(event) =>
              setCommandForbiddenArgvPrefixes(event.currentTarget.value)
            }
            value={commandForbiddenArgvPrefixes}
          />
        </label>
      </div>
      <label className="field">
        <span>CLI tool names</span>
        <textarea
          className="config-small-textarea"
          data-testid="tool-cli-tool-names"
          onChange={(event) => setCliToolNames(event.currentTarget.value)}
          value={cliToolNames}
        />
      </label>
      <div className="grid-2">
        <label className="checkbox">
          <input
            checked={enableMetaTools}
            data-testid="tool-enable-meta-tools"
            onChange={(event) => setEnableMetaTools(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Meta tools</span>
        </label>
        <div className="field">
          <span>MCP service allowlist</span>
          <div className="linked-doc-list" data-testid="tool-allowed-mcp-services">
            {toolServiceRegistries.map((service) => {
              const serviceId = service.serviceId;
              const checked = linesToArray(allowedMcpServiceIds).includes(serviceId);
              return (
                <label className="checkbox" key={serviceId}>
                  <input
                    checked={checked}
                    data-testid={`tool-allowed-mcp-service-${serviceId}`}
                    disabled={!enableMetaTools}
                    onChange={(event) =>
                      toggleAllowedMcpService(serviceId, event.currentTarget.checked)
                    }
                    type="checkbox"
                  />
                  <span>{service.displayName ?? serviceId}</span>
                </label>
              );
            })}
            {!toolServiceRegistries.length ? (
              <p className="muted">Create an HTTP MCP service first.</p>
            ) : null}
          </div>
        </div>
      </div>
      <label className="field">
        <span>Delegate agent DIDs</span>
        <textarea
          className="config-small-textarea"
          data-testid="tool-delegate-to"
          onChange={(event) => setDelegateTo(event.currentTarget.value)}
          value={delegateTo}
        />
      </label>
      <label className="field">
        <span>Backgroundable tool names</span>
        <textarea
          className="config-small-textarea"
          data-testid="tool-backgroundable-tool-names"
          onChange={(event) => setBackgroundableToolNames(event.currentTarget.value)}
          value={backgroundableToolNames}
        />
      </label>
      <div className="grid-2">
        <label className="field">
          <span>Subagent targets</span>
          <textarea
            className="config-small-textarea"
            data-testid="tool-subagent-targets"
            onChange={(event) => setSubagentTargets(event.currentTarget.value)}
            value={subagentTargets}
          />
        </label>
        <label className="field">
          <span>Cross-deployment spawn timeout</span>
          <input
            aria-invalid={!crossDeploymentSpawnTimeoutValid}
            data-testid="tool-cross-deployment-spawn-timeout"
            inputMode="numeric"
            onChange={(event) =>
              setCrossDeploymentSpawnTimeoutSeconds(event.currentTarget.value)
            }
            value={crossDeploymentSpawnTimeoutSeconds}
          />
        </label>
      </div>
      <div className="grid-2">
        <label className="checkbox">
          <input
            checked={subagentSpawnEnabled}
            data-testid="tool-subagent-spawn-enabled"
            onChange={(event) => setSubagentSpawnEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Subagent spawn</span>
        </label>
        <label className="checkbox">
          <input
            checked={subagentSteeringEnabled}
            data-testid="tool-subagent-steering-enabled"
            onChange={(event) =>
              setSubagentSteeringEnabled(event.currentTarget.checked)
            }
            type="checkbox"
          />
          <span>Subagent steering</span>
        </label>
        <label className="checkbox">
          <input
            checked={subagentBackgroundEnabled}
            data-testid="tool-subagent-background-enabled"
            onChange={(event) =>
              setSubagentBackgroundEnabled(event.currentTarget.checked)
            }
            type="checkbox"
          />
          <span>Subagent background</span>
        </label>
      </div>
      <div className="config-actions">
        <button
          className="primary-button"
          data-testid="tool-selection-save"
          disabled={
            saving ||
            !selectionId.trim() ||
            !displayName.trim() ||
            !crossDeploymentSpawnTimeoutValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Tool Selection"}
        </button>
      </div>
    </form>
  );
}

function normalizeToolCeiling(value?: string | null) {
  switch ((value ?? "").toLowerCase()) {
    case "metaonly":
    case "meta-only":
      return "MetaOnly";
    case "readwrite":
    case "read-write":
      return "Readwrite";
    case "readonly":
    case "read-only":
      return "Readonly";
    default:
      return "Unknown";
  }
}

function normalizeCommandExecutionPolicy(value?: string | null) {
  switch ((value ?? "").trim().toLowerCase()) {
    case "":
      return "";
    case "readonly":
    case "read_only":
      return "read_only";
    case "managedwrite":
    case "managed_write":
    case "workspacewrite":
    case "workspace_write":
      return "workspace_write";
    case "unrestricted":
      return "unrestricted";
    default:
      return "";
  }
}

function normalizeCommandNetworkMode(value?: string | null) {
  switch ((value ?? "").trim().toLowerCase()) {
    case "":
    case "inherit":
      return "";
    case "off":
    case "disabled":
      return "disabled";
    case "on":
    case "enabled":
      return "enabled";
    default:
      return "";
  }
}

function displayToolCeiling(value: ReturnType<typeof normalizeToolCeiling>) {
  switch (value) {
    case "MetaOnly":
      return "Meta only";
    case "Readwrite":
      return "Read/write";
    case "Readonly":
      return "Read only";
    default:
      return "unknown";
  }
}
