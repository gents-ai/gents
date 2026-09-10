import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type {
  ConfigComponentsApplyRequest,
  DeploymentView,
  Tools,
  ToolsDeleteRequest,
  ToolServiceRegistry,
  SubagentTargetDocument,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";

export type ToolsConfigPanelProps = {
  deployment: DeploymentView;
  selectedToolsId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectTools: (id: string) => void;
  onCreateTools: () => void;
  onSavedStatusChange: (value: string) => void;
  onApplyConfigComponents: (request: ConfigComponentsApplyRequest) => Promise<unknown>;
  onDeleteToolsConfig: (request: ToolsDeleteRequest) => Promise<unknown>;
  onDeletedTools: () => void;
  toolCeiling?: string | null;
  toolRoot?: string | null;
};
export function ToolsConfigPanel({
  deployment,
  selectedToolsId,
  saving,
  savedStatus,
  onSelectTools,
  onCreateTools,
  onSavedStatusChange,
  onApplyConfigComponents,
  onDeleteToolsConfig,
  onDeletedTools,
  toolCeiling,
  toolRoot,
}: ToolsConfigPanelProps) {
  const selected =
    deployment.tools.find((entry) => entry.tools_id === selectedToolsId) ?? null;
  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Tools"
        title="Tools"
        items={deployment.tools.map((entry) => ({
          id: entry.tools_id,
          title: entry.display_name ?? entry.tools_id,
          meta: entry.host ? "Host tools" : "Tools",
        }))}
        selectedId={selectedToolsId}
        testPrefix="tools"
        onCreate={onCreateTools}
        onSelect={onSelectTools}
      />
      <ToolsConfigEditor
        agentDid={deployment.agentDid}
        tools={selected}
        toolServices={deployment.toolServiceRegistries}
        subagentTargets={deployment.subagentTargets}
        saving={saving}
        savedStatus={savedStatus}
        toolCeiling={toolCeiling}
        toolRoot={toolRoot}
        onApplyConfigComponents={onApplyConfigComponents}
        onDeleteToolsConfig={onDeleteToolsConfig}
        onDeleted={onDeletedTools}
        onSaved={(id) => {
          onSelectTools(id);
          onSavedStatusChange(`tool:${id}`);
        }}
      />
    </section>
  );
}
export type ToolsConfigEditorProps = {
  agentDid: string;
  tools: Tools | null;
  toolServices: ToolServiceRegistry[];
  subagentTargets: SubagentTargetDocument[];
  saving: boolean;
  savedStatus: string | null;
  toolCeiling?: string | null;
  toolRoot?: string | null;
  onApplyConfigComponents: (request: ConfigComponentsApplyRequest) => Promise<unknown>;
  onDeleteToolsConfig: (request: ToolsDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
  onSaved: (id: string) => void;
};
const lines = (text: string) => text.split("\n").filter((value) => value.length > 0);
export function ToolsConfigEditor({
  agentDid,
  tools,
  toolServices,
  subagentTargets,
  saving,
  savedStatus,
  toolCeiling,
  toolRoot,
  onApplyConfigComponents,
  onDeleteToolsConfig,
  onDeleted,
  onSaved,
}: ToolsConfigEditorProps) {
  const initial = () => tools ?? { agent_did: agentDid, tools_id: "" };
  const [draft, setDraft] = useState<Tools>(initial);
  const [json, setJson] = useState("");
  const [targetsJson, setTargetsJson] = useState("[]");
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  useEffect(() => {
    const value = initial();
    setDraft(value);
    setJson(JSON.stringify(value, null, 2));
    setError(null);
    const ids = value.subagents?.target_ids ?? [];
    setTargetsJson(
      JSON.stringify(
        subagentTargets.filter((target) => ids.includes(target.target_id)),
        null,
        2,
      ),
    );
  }, [tools?.tools_id, agentDid]);
  function update(value: Tools) {
    setDraft(value);
    setJson(JSON.stringify(value, null, 2));
  }
  function updateHost(host: NonNullable<Tools["host"]>) {
    update({ ...draft, host });
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    setError(null);
    try {
      const document = JSON.parse(json) as Tools;
      if (document.agent_did !== agentDid)
        throw new Error("Tools owner must match the selected principal.");
      if (tools && document.tools_id !== tools.tools_id)
        throw new Error("Tools IDs cannot be renamed.");
      const targets = JSON.parse(targetsJson) as SubagentTargetDocument[];
      if (!Array.isArray(targets))
        throw new Error("Subagent targets must be an array of canonical documents.");
      await onApplyConfigComponents({
        document: {
          agent_principal: { agent_did: agentDid },
          tools: [document],
          subagent_targets: targets,
        },
      });
      update(document);
      onSaved(document.tools_id);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }
  function applyJson() {
    try {
      const value = JSON.parse(json) as Tools;
      if (!value || Array.isArray(value) || typeof value !== "object")
        throw new Error("Tools must be a document.");
      if (value.agent_did !== agentDid || (tools && value.tools_id !== tools.tools_id))
        throw new Error("Tools identity must match the selected document.");
      update(value);
      setError(null);
    } catch (caught) {
      setError(String(caught));
    }
  }
  return (
    <form className="panel config-editor" onSubmit={save}>
      <ConfigEditorHeader
        eyebrow="Tools"
        title={draft.display_name || draft.tools_id || "New tools"}
        saved={savedStatus === `tool:${draft.tools_id}`}
        dirty={json !== JSON.stringify(initial(), null, 2)}
      />
      {error ? <FieldHint show>{error}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Tools ID</span>
          <input
            data-testid="tools-id"
            value={draft.tools_id}
            readOnly={Boolean(tools)}
            onChange={(event) => {
              if (!tools) update({ ...draft, tools_id: event.currentTarget.value });
            }}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="tools-display-name"
            value={draft.display_name ?? ""}
            onChange={(event) =>
              update({ ...draft, display_name: event.currentTarget.value || null })
            }
          />
        </label>
      </div>
      <h3>Host</h3>
      {toolCeiling ? (
        <p>
          Current runtime tool ceiling: {toolCeiling}. Runtime authority still applies
          when tools execute.
        </p>
      ) : null}
      <label className="field">
        <span>Working directory</span>
        <input
          data-testid="tools-root"
          value={draft.host?.root ?? ""}
          placeholder={toolRoot ?? "Runtime working directory"}
          onChange={(event) =>
            updateHost({ ...draft.host, root: event.currentTarget.value || null })
          }
        />
      </label>
      <div className="grid-2">
        <label className="field">
          <span>Files</span>
          <select
            data-testid="tools-files-mode"
            value={draft.host?.files?.mode ?? "Off"}
            onChange={(event) =>
              updateHost({
                ...draft.host,
                files: {
                  ...draft.host?.files,
                  mode: event.currentTarget.value as NonNullable<
                    NonNullable<Tools["host"]>["files"]
                  >["mode"],
                },
              })
            }
          >
            <option value="Off">Off</option>
            <option value="ReadOnly">Read only</option>
            <option value="ReadWrite">Read and write</option>
          </select>
        </label>
        <label className="field">
          <span>Bash</span>
          <select
            data-testid="tools-bash-mode"
            value={draft.host?.bash?.mode ?? "Off"}
            onChange={(event) =>
              updateHost({
                ...draft.host,
                bash: {
                  ...draft.host?.bash,
                  mode: event.currentTarget.value as NonNullable<
                    NonNullable<Tools["host"]>["bash"]
                  >["mode"],
                },
              })
            }
          >
            <option value="Off">Off</option>
            <option value="ReadOnly">Read only</option>
            <option value="Unrestricted">Unrestricted</option>
          </select>
        </label>
      </div>
      <label className="checkbox">
        <input
          data-testid="tools-bash-background"
          type="checkbox"
          checked={draft.host?.bash?.background_enabled ?? false}
          onChange={(event) =>
            updateHost({
              ...draft.host,
              bash: {
                ...draft.host?.bash,
                background_enabled: event.currentTarget.checked,
              },
            })
          }
        />
        <span>Allow background bash</span>
      </label>
      <h3>Remote services</h3>
      <p>
        Choose explicit tool names for each service. Presentation changes how selected
        tools appear, not which tools are allowed.
      </p>
      {toolServices.map((service) => {
        const grant = draft.remote?.services?.find(
          (entry) => entry.mcp_service_id === service.service_id,
        );
        function setGrant(
          value: NonNullable<NonNullable<Tools["remote"]>["services"]>[number] | null,
        ) {
          const services = (draft.remote?.services ?? []).filter(
            (entry) => entry.mcp_service_id !== service.service_id,
          );
          if (value) services.push(value);
          update({ ...draft, remote: { ...draft.remote, services } });
        }
        return (
          <fieldset key={service.service_id}>
            <legend>{service.display_name ?? service.service_id}</legend>
            <label className="checkbox">
              <input
                data-testid={`tools-service-${service.service_id}`}
                type="checkbox"
                checked={Boolean(grant)}
                onChange={(event) =>
                  setGrant(
                    event.currentTarget.checked
                      ? { mcp_service_id: service.service_id, tool_names: [] }
                      : null,
                  )
                }
              />
              <span>Configure this service</span>
            </label>
            {grant ? (
              <>
                <label className="field">
                  <span>Allowed tool names, one per line</span>
                  <textarea
                    data-testid={`tools-service-names-${service.service_id}`}
                    value={(grant.tool_names ?? []).join("\n")}
                    onChange={(event) =>
                      setGrant({
                        ...grant,
                        tool_names: lines(event.currentTarget.value),
                      })
                    }
                  />
                </label>
                <label className="field">
                  <span>Presentation</span>
                  <select
                    data-testid={`tools-service-style-${service.service_id}`}
                    value={grant.style ?? "discovery"}
                    onChange={(event) =>
                      setGrant({
                        ...grant,
                        style: event.currentTarget.value as "flat" | "discovery",
                      })
                    }
                  >
                    <option value="discovery">Discovery tools</option>
                    <option value="flat">Flat tool list</option>
                  </select>
                </label>
              </>
            ) : null}
          </fieldset>
        );
      })}
      <h3>Subagents</h3>
      <label className="field">
        <span>Selected target IDs, one per line</span>
        <textarea
          data-testid="tools-target-ids"
          value={(draft.subagents?.target_ids ?? []).join("\n")}
          onChange={(event) =>
            update({
              ...draft,
              subagents: {
                ...draft.subagents,
                target_ids: lines(event.currentTarget.value),
              },
            })
          }
        />
      </label>
      <label className="field">
        <span>Target documents</span>
        <textarea
          data-testid="tools-target-documents"
          value={targetsJson}
          onChange={(event) => setTargetsJson(event.currentTarget.value)}
        />
        <span>
          Targets are canonical documents with explicit owner, destination, behavior,
          and callable name. Defining one does not grant access until its ID is
          selected.
        </span>
      </label>
      <h3>Built-ins</h3>
      <label className="checkbox">
        <input
          data-testid="tools-goal-tools"
          type="checkbox"
          checked={draft.built_ins?.enable_goal_tools ?? false}
          onChange={(event) =>
            update({
              ...draft,
              built_ins: {
                ...draft.built_ins,
                enable_goal_tools: event.currentTarget.checked,
              },
            })
          }
        />
        <span>Goal tools</span>
      </label>
      <label className="checkbox">
        <input
          data-testid="tools-goal-creation"
          type="checkbox"
          checked={draft.built_ins?.enable_goal_creation ?? false}
          onChange={(event) =>
            update({
              ...draft,
              built_ins: {
                ...draft.built_ins,
                enable_goal_creation: event.currentTarget.checked,
              },
            })
          }
        />
        <span>Goal creation</span>
      </label>
      <details>
        <summary>All tool settings</summary>
        <p>
          Edit the canonical document, including timeouts, command rules, datastore
          surfaces, integrations, and self-configuration. Omitted groups grant no tools.
        </p>
        <textarea
          data-testid="tools-document"
          className="config-textarea"
          value={json}
          onChange={(event) => setJson(event.currentTarget.value)}
        />
        <button type="button" onClick={applyJson}>
          Update controls
        </button>
      </details>
      <div className="config-actions">
        {tools ? (
          <button
            type="button"
            className="ghost-button danger-button"
            disabled={saving}
            onClick={() => setConfirmDelete(true)}
          >
            Delete tools
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmDelete}
          title="Delete tools?"
          message="Existing context references must be removed before deletion."
          confirmLabel="Delete"
          onConfirm={() => {
            setConfirmDelete(false);
            if (tools)
              void onDeleteToolsConfig({ agentDid, toolsId: tools.tools_id })
                .then(onDeleted)
                .catch((caught) => setError(String(caught)));
          }}
          onCancel={() => setConfirmDelete(false)}
        />
        <button
          data-testid="tools-save"
          className="primary-button"
          type="submit"
          disabled={saving || !draft.tools_id.trim()}
        >
          {saving ? "Saving..." : "Save tools"}
        </button>
      </div>
    </form>
  );
}
