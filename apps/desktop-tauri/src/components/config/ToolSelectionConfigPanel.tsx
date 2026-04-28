import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ToolSelectionSaveRequest,
  ToolSelectionView,
  ToolServiceRegistryView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import { linesToArray } from "./formUtils";

export type ToolSelectionConfigPanelProps = {
  deployment: DeploymentView;
  selectedToolSelectionId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectToolSelection: (selectionId: string) => void;
  onCreateToolSelection: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveToolSelectionConfig: (
    request: ToolSelectionSaveRequest,
  ) => Promise<unknown>;
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
  onSaveToolSelectionConfig: (
    request: ToolSelectionSaveRequest,
  ) => Promise<unknown>;
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
  const [cliToolNames, setCliToolNames] = useState("");
  const [enableMetaTools, setEnableMetaTools] = useState(false);
  const [delegateTo, setDelegateTo] = useState("");
  const ceiling = normalizeToolCeiling(toolCeiling);
  const fileToolsDisabledByCeiling = ceiling === "MetaOnly";
  const writeToolsDisabledByCeiling = ceiling !== "Readwrite";

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
        : toolSelection?.bashMode ?? "ReadOnly",
    );
    setCliToolNames((toolSelection?.cliToolNames ?? []).join("\n"));
    setEnableMetaTools(toolSelection?.enableMetaTools ?? false);
    setDelegateTo((toolSelection?.delegateTo ?? []).join("\n"));
  }, [toolSelection]);

  function toggleDelegateTo(serviceId: string, checked: boolean) {
    const values = new Set(linesToArray(delegateTo));
    if (checked) {
      values.add(serviceId);
    } else {
      values.delete(serviceId);
    }
    setDelegateTo(Array.from(values).sort().join("\n"));
  }

  async function submitToolSelection(event: FormEvent) {
    event.preventDefault();
    const nextId = selectionId.trim();
    const effectiveEnableFileTools =
      !fileToolsDisabledByCeiling && enableFileTools;
    const effectiveEnableBash = !fileToolsDisabledByCeiling && enableBash;
    const effectiveFileToolsMode =
      writeToolsDisabledByCeiling && fileToolsMode === "ReadWrite"
        ? "ReadOnly"
        : fileToolsMode;
    const effectiveBashMode =
      writeToolsDisabledByCeiling && bashMode === "Unrestricted"
        ? "ReadOnly"
        : bashMode;
    await onSaveToolSelectionConfig({
      agentDid,
      selectionId: nextId,
      displayName,
      enableFileTools: effectiveEnableFileTools,
      fileToolsMode: effectiveFileToolsMode,
      fileToolRoot,
      enableBash: effectiveEnableBash,
      bashMode: effectiveBashMode,
      cliToolNames: linesToArray(cliToolNames),
      enableMetaTools,
      delegateTo: linesToArray(delegateTo),
    });
    onSaved(nextId);
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
            onChange={(event) => setSelectionId(event.currentTarget.value)}
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
          <span>Linked HTTP MCP services</span>
          <div className="linked-doc-list" data-testid="tool-delegate-to">
            {toolServiceRegistries.map((service) => {
              const serviceId = service.serviceId;
              const checked = linesToArray(delegateTo).includes(serviceId);
              return (
                <label className="checkbox" key={serviceId}>
                  <input
                    checked={checked}
                    data-testid={`tool-delegate-${serviceId}`}
                    disabled={!enableMetaTools}
                    onChange={(event) =>
                      toggleDelegateTo(serviceId, event.currentTarget.checked)
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
      <div className="config-actions">
        <button
          className="primary-button"
          data-testid="tool-selection-save"
          disabled={saving || !selectionId.trim() || !displayName.trim()}
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
