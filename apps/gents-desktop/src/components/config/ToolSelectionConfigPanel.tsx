import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ToolSelectionDeleteRequest,
  ToolSelectionSaveRequest,
  ToolSelectionView,
  ToolServiceRegistryView,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { isOptionalInt, linesToArray, parseOptionalInt } from "./formUtils";

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

type NullableBooleanMode = "inherit" | "enabled" | "disabled";

const GOAL_TOOLS_OPTIONS = [
  { value: "inherit", label: "Inherit meta tools" },
  { value: "enabled", label: "Enabled" },
  { value: "disabled", label: "Disabled" },
] as const;

const GOAL_CREATION_OPTIONS = [
  { value: "inherit", label: "Default (disabled)" },
  { value: "enabled", label: "Enabled" },
  { value: "disabled", label: "Disabled" },
] as const;

function nullableBooleanMode(value?: boolean | null): NullableBooleanMode {
  if (value == null) return "inherit";
  return value ? "enabled" : "disabled";
}

function nullableBooleanValue(value: NullableBooleanMode): boolean | null {
  if (value === "inherit") return null;
  return value === "enabled";
}

export type ToolSelectionConfigPanelProps = {
  deployment: DeploymentView;
  selectedToolSelectionId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectToolSelection: (selectionId: string) => void;
  onCreateToolSelection: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveToolSelectionConfig: (request: ToolSelectionSaveRequest) => Promise<unknown>;
  onDeleteToolSelectionConfig: (
    request: ToolSelectionDeleteRequest,
  ) => Promise<unknown>;
  onDeletedToolSelection: () => void;
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
  onDeleteToolSelectionConfig,
  onDeletedToolSelection,
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
        onDeleteToolSelectionConfig={onDeleteToolSelectionConfig}
        onDeleted={() => {
          onDeletedToolSelection();
        }}
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
  onDeleteToolSelectionConfig: (
    request: ToolSelectionDeleteRequest,
  ) => Promise<unknown>;
  onDeleted: () => void;
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
  onDeleteToolSelectionConfig,
  onDeleted,
}: ToolSelectionConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteToolSelection() {
    setConfirmingDelete(false);
    if (!toolSelection) {
      return;
    }
    try {
      await onDeleteToolSelectionConfig({
        selectionId: toolSelection.selectionId,
        agentDid: toolSelection.agentDid ?? agentDid,
      });
      onDeleted();
    } catch {}
  }
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
  const [goalToolsMode, setGoalToolsMode] = useState<NullableBooleanMode>("inherit");
  const [goalCreationMode, setGoalCreationMode] =
    useState<NullableBooleanMode>("inherit");
  const [allowedMcpServiceIds, setAllowedMcpServiceIds] = useState("");
  const [backgroundableToolNames, setBackgroundableToolNames] = useState("");
  const [subagentTargets, setSubagentTargets] = useState("");
  const [subagentSpawnEnabled, setSubagentSpawnEnabled] = useState(false);
  const [subagentSteeringEnabled, setSubagentSteeringEnabled] = useState(false);
  const [subagentBackgroundEnabled, setSubagentBackgroundEnabled] = useState(false);
  const [crossDeploymentSpawnTimeoutSeconds, setCrossDeploymentSpawnTimeoutSeconds] =
    useState("");
  const [defraQueryCollections, setDefraQueryCollections] = useState("");
  const ceiling = normalizeToolCeiling(toolCeiling);
  const fileToolsDisabledByCeiling = ceiling === "MetaOnly";
  const writeToolsDisabledByCeiling = ceiling !== "Readwrite";
  const crossDeploymentSpawnTimeoutValid = isOptionalInt(
    crossDeploymentSpawnTimeoutSeconds,
    { min: 1 },
  );
  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const b = toolSelectionFormValues(toolSelection);
    setSelectionId(b.selectionId);
    setDisplayName(b.displayName);
    setEnableFileTools(b.enableFileTools);
    setFileToolsMode(b.fileToolsMode);
    setFileToolRoot(b.fileToolRoot);
    setEnableBash(b.enableBash);
    setBashMode(b.bashMode);
    setCommandExecutionPolicy(b.commandExecutionPolicy);
    setCommandAllowedArgvPrefixes(b.commandAllowedArgvPrefixes);
    setCommandForbiddenArgvPrefixes(b.commandForbiddenArgvPrefixes);
    setCommandNetworkMode(b.commandNetworkMode);
    setCliToolNames(b.cliToolNames);
    setEnableMetaTools(b.enableMetaTools);
    setGoalToolsMode(b.goalToolsMode);
    setGoalCreationMode(b.goalCreationMode);
    setAllowedMcpServiceIds(b.allowedMcpServiceIds);
    setBackgroundableToolNames(b.backgroundableToolNames);
    setSubagentTargets(b.subagentTargets);
    setSubagentSpawnEnabled(b.subagentSpawnEnabled);
    setSubagentSteeringEnabled(b.subagentSteeringEnabled);
    setSubagentBackgroundEnabled(b.subagentBackgroundEnabled);
    setCrossDeploymentSpawnTimeoutSeconds(b.crossDeploymentSpawnTimeoutSeconds);
    setDefraQueryCollections(b.defraQueryCollections);
    setSaveError(null);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toolSelection?.selectionId]);

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
        defraQueryCollections: linesToArray(defraQueryCollections),
        enableMetaTools,
        enableGoalTools: nullableBooleanValue(goalToolsMode),
        enableGoalCreation: nullableBooleanValue(goalCreationMode),
        allowedMcpServiceIds: linesToArray(allowedMcpServiceIds),
        requiredMcpServiceIds: toolSelection?.requiredMcpServiceIds ?? [],
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
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitToolSelection}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            selectionId,
            displayName,
            enableFileTools,
            fileToolsMode,
            fileToolRoot,
            enableBash,
            bashMode,
            commandExecutionPolicy,
            commandAllowedArgvPrefixes,
            commandForbiddenArgvPrefixes,
            commandNetworkMode,
            cliToolNames,
            enableMetaTools,
            goalToolsMode,
            goalCreationMode,
            allowedMcpServiceIds,
            backgroundableToolNames,
            subagentTargets,
            subagentSpawnEnabled,
            subagentSteeringEnabled,
            subagentBackgroundEnabled,
            crossDeploymentSpawnTimeoutSeconds,
            defraQueryCollections,
          },
          toolSelectionFormValues(toolSelection),
        )}
        eyebrow="Tool Selection"
        saved={savedStatus === `tool:${selectionId.trim()}`}
        title={displayName || selectionId || "New Tool Selection"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
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
        <div>
          <dt>Policy version</dt>
          <dd className="mono" data-testid="tool-policy-version">
            {toolSelection?.toolPolicyVersion ?? "missing (runtime rejects)"}
          </dd>
        </div>
        <div>
          <dt>Managed write tools</dt>
          <dd className="mono" data-testid="tool-write-tools">
            {toolSelection?.writeTools?.length
              ? toolSelection.writeTools.map(describeWriteTool).join(", ")
              : "none"}
          </dd>
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
      <label className="field">
        <span>Defra query collections</span>
        <textarea
          className="config-small-textarea"
          data-testid="tool-defra-query-collections"
          onChange={(event) => setDefraQueryCollections(event.currentTarget.value)}
          placeholder="One collection per line (empty = all collections)"
          value={defraQueryCollections}
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
        <label className="field">
          <span>Goal lifecycle tools</span>
          <select
            data-testid="tool-enable-goal-tools"
            onChange={(event) =>
              setGoalToolsMode(event.currentTarget.value as NullableBooleanMode)
            }
            value={goalToolsMode}
          >
            {GOAL_TOOLS_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Model goal creation</span>
          <select
            data-testid="tool-enable-goal-creation"
            onChange={(event) =>
              setGoalCreationMode(event.currentTarget.value as NullableBooleanMode)
            }
            value={goalCreationMode}
          >
            {GOAL_CREATION_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
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
          <FieldHint show={!crossDeploymentSpawnTimeoutValid}>
            Whole number of 1 or more
          </FieldHint>
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
        {toolSelection ? (
          <button
            className="ghost-button danger-button"
            data-testid="tool-selection-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Selection
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete tool selection"
          message={`Delete tool selection "${toolSelection?.selectionId ?? ""}"? Behaviors still pointing at it will block the delete.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            void deleteToolSelection();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
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

function describeWriteTool(decl: string): string {
  try {
    const parsed = JSON.parse(decl) as {
      tool_name?: unknown;
      collection?: unknown;
    };
    const name = typeof parsed?.tool_name === "string" ? parsed.tool_name : null;
    const collection =
      typeof parsed?.collection === "string" ? parsed.collection : null;
    if (name) {
      return collection ? `${name} → ${collection}` : name;
    }
  } catch {}
  return decl;
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

function toolSelectionFormValues(toolSelection: ToolSelectionView | null) {
  const existingAllowedServiceIds = toolSelection?.allowedMcpServiceIds ?? [];
  return {
    selectionId: toolSelection?.selectionId ?? "",
    displayName: toolSelection?.displayName ?? toolSelection?.selectionId ?? "",
    enableFileTools: toolSelection?.enableFileTools ?? false,
    fileToolsMode: toolSelection?.fileToolsMode ?? "ReadOnly",
    fileToolRoot: toolSelection?.fileToolRoot ?? "",
    enableBash: toolSelection?.enableBash ?? false,
    bashMode:
      toolSelection?.bashMode === "ReadWrite"
        ? "Unrestricted"
        : (toolSelection?.bashMode ?? "ReadOnly"),
    commandExecutionPolicy: normalizeCommandExecutionPolicy(
      toolSelection?.commandExecutionPolicy,
    ),
    commandAllowedArgvPrefixes: (toolSelection?.commandAllowedArgvPrefixes ?? []).join(
      "\n",
    ),
    commandForbiddenArgvPrefixes: (
      toolSelection?.commandForbiddenArgvPrefixes ?? []
    ).join("\n"),
    commandNetworkMode: normalizeCommandNetworkMode(toolSelection?.commandNetworkMode),
    cliToolNames: (toolSelection?.cliToolNames ?? []).join("\n"),
    enableMetaTools: toolSelection?.enableMetaTools ?? false,
    goalToolsMode: nullableBooleanMode(toolSelection?.enableGoalTools),
    goalCreationMode: nullableBooleanMode(toolSelection?.enableGoalCreation),
    allowedMcpServiceIds: existingAllowedServiceIds.join("\n"),
    backgroundableToolNames: (toolSelection?.backgroundableToolNames ?? []).join("\n"),
    subagentTargets: (toolSelection?.subagentTargets ?? []).join("\n"),
    subagentSpawnEnabled: toolSelection?.subagentSpawnEnabled ?? false,
    subagentSteeringEnabled: toolSelection?.subagentSteeringEnabled ?? false,
    subagentBackgroundEnabled: toolSelection?.subagentBackgroundEnabled ?? false,
    crossDeploymentSpawnTimeoutSeconds:
      toolSelection?.crossDeploymentSpawnTimeoutSeconds != null
        ? String(toolSelection.crossDeploymentSpawnTimeoutSeconds)
        : "",
    defraQueryCollections: (toolSelection?.defraQueryCollections ?? []).join("\n"),
  };
}
