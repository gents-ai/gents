import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type {
  AgentBehavior,
  AgentContext,
  AgentPrincipal,
  CompactionConfig,
  AgentConfigSaveRequest,
  BehaviorDeleteRequest,
  BehaviorView,
  ConfigComponentsApplyRequest,
  DeploymentView,
  DesktopApiAdapter,
  InferenceProfile,
  SkillView,
  Tools,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { BehaviorToolSurface } from "./BehaviorToolSurface";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { isOptionalFloat, parseOptionalFloat } from "./formUtils";

export type BehaviorConfigPanelProps = {
  api: DesktopApiAdapter;
  deployment: DeploymentView;
  selectedBehavior: BehaviorView | null;
  saving: boolean;
  savedStatus: string | null;
  onSavedStatusChange: (value: string) => void;
  onCreateBehavior: () => void;
  onCreateProfile: () => void;
  onCreateTools: () => void;
  onSelectBehavior: (id: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onApplyConfigComponents: (request: ConfigComponentsApplyRequest) => Promise<unknown>;
  onDeleteBehaviorConfig: (request: BehaviorDeleteRequest) => Promise<unknown>;
  onDeletedBehavior: () => void;
};
export function BehaviorConfigPanel({
  api,
  deployment,
  selectedBehavior,
  saving,
  savedStatus,
  onSavedStatusChange,
  onCreateBehavior,
  onCreateProfile,
  onCreateTools,
  onSelectBehavior,
  onSaveAgentConfig,
  onApplyConfigComponents,
  onDeleteBehaviorConfig,
  onDeletedBehavior,
}: BehaviorConfigPanelProps) {
  const config =
    deployment.behaviorConfigs.find(
      (entry) => entry.behavior_id === selectedBehavior?.behaviorId,
    ) ?? null;
  if (selectedBehavior && !config)
    return <FieldHint show>Behavior configuration is not available.</FieldHint>;
  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Behaviors"
        title="Agent behaviors"
        items={deployment.behaviorConfigs.map((entry) => ({
          id: entry.behavior_id,
          title: entry.display_name ?? entry.behavior_id,
          meta:
            deployment.agentPrincipal.defaultBehaviorId === entry.behavior_id
              ? "default"
              : entry.enabled === false
                ? "disabled"
                : "enabled",
        }))}
        selectedId={selectedBehavior?.behaviorId ?? null}
        testPrefix="behavior"
        onCreate={onCreateBehavior}
        onSelect={onSelectBehavior}
      />
      <BehaviorConfigEditor
        api={api}
        agentDid={deployment.agentDid}
        principal={deployment.principalConfig}
        behavior={config}
        contexts={deployment.contexts}
        compactions={deployment.compactions}
        inferenceProfiles={deployment.inferenceProfiles}
        tools={deployment.tools}
        skills={deployment.skills}
        saving={saving}
        savedStatus={savedStatus}
        onCreateProfile={onCreateProfile}
        onCreateTools={onCreateTools}
        onSaveAgentConfig={onSaveAgentConfig}
        onApplyConfigComponents={onApplyConfigComponents}
        onDeleteBehaviorConfig={onDeleteBehaviorConfig}
        onDeleted={onDeletedBehavior}
        onSaved={(id) => {
          onSelectBehavior(id);
          onSavedStatusChange(`behavior:${id}`);
        }}
      />
    </section>
  );
}
export type BehaviorConfigEditorProps = {
  api: DesktopApiAdapter;
  agentDid: string;
  principal: AgentPrincipal | null;
  behavior: AgentBehavior | null;
  contexts: AgentContext[];
  compactions: CompactionConfig[];
  inferenceProfiles: InferenceProfile[];
  tools: Tools[];
  skills: SkillView[];
  saving: boolean;
  savedStatus: string | null;
  onCreateProfile: () => void;
  onCreateTools: () => void;
  onSaved: (id: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onApplyConfigComponents: (request: ConfigComponentsApplyRequest) => Promise<unknown>;
  onDeleteBehaviorConfig: (request: BehaviorDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
};
export function BehaviorConfigEditor({
  api,
  agentDid,
  principal,
  behavior,
  contexts,
  compactions,
  inferenceProfiles,
  tools,
  skills,
  saving,
  savedStatus,
  onCreateProfile,
  onCreateTools,
  onSaved,
  onSaveAgentConfig,
  onApplyConfigComponents,
  onDeleteBehaviorConfig,
  onDeleted,
}: BehaviorConfigEditorProps) {
  const [behaviorId, setBehaviorId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [profileId, setProfileId] = useState("");
  const [contextId, setContextId] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [toolsId, setToolsId] = useState("");
  const [compactionId, setCompactionId] = useState("");
  const [strategy, setStrategy] =
    useState<CompactionConfig["strategy"]>("StripThenSummarize");
  const [threshold, setThreshold] = useState("");
  const [skillIds, setSkillIds] = useState<string[]>([]);
  const [enabled, setEnabled] = useState(true);
  const [defaultForAgent, setDefaultForAgent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  function loadCompaction(id: string) {
    const selected = compactions.find((entry) => entry.compaction_id === id);
    setCompactionId(id);
    setStrategy(selected?.strategy ?? "StripThenSummarize");
    setThreshold(selected?.threshold == null ? "" : String(selected.threshold));
  }
  function loadContext(id: string) {
    const selected = contexts.find((entry) => entry.context_id === id);
    setContextId(id);
    setSystemPrompt(selected?.system_prompt ?? "");
    setToolsId(selected?.tools_id ?? "");
    setSkillIds(selected?.skill_ids ?? []);
    loadCompaction(selected?.compaction_id ?? "");
  }
  useEffect(() => {
    setBehaviorId(behavior?.behavior_id ?? "");
    setDisplayName(behavior?.display_name ?? "");
    setProfileId(behavior?.inference_profile_id ?? "");
    setEnabled(behavior?.enabled ?? true);
    setDefaultForAgent(
      Boolean(behavior && principal?.default_behavior_id === behavior.behavior_id),
    );
    loadContext(behavior?.context_id ?? "");
    setError(null);
  }, [behavior?.behavior_id, agentDid]);
  const context = contexts.find((entry) => entry.context_id === contextId);
  const compaction = compactions.find((entry) => entry.compaction_id === compactionId);
  const profile = inferenceProfiles.find((entry) => entry.profile_id === profileId);
  const originalContext = contexts.find(
    (entry) => entry.context_id === behavior?.context_id,
  );
  const originalCompaction = compactions.find(
    (entry) => entry.compaction_id === originalContext?.compaction_id,
  );
  const dirty =
    behaviorId !== (behavior?.behavior_id ?? "") ||
    displayName !== (behavior?.display_name ?? "") ||
    profileId !== (behavior?.inference_profile_id ?? "") ||
    contextId !== (behavior?.context_id ?? "") ||
    systemPrompt !== (originalContext?.system_prompt ?? "") ||
    toolsId !== (originalContext?.tools_id ?? "") ||
    compactionId !== (originalContext?.compaction_id ?? "") ||
    strategy !== (originalCompaction?.strategy ?? "StripThenSummarize") ||
    threshold !==
      (originalCompaction?.threshold == null
        ? ""
        : String(originalCompaction.threshold)) ||
    enabled !== (behavior?.enabled ?? true) ||
    defaultForAgent !==
      Boolean(behavior && principal?.default_behavior_id === behavior.behavior_id) ||
    JSON.stringify([...skillIds].sort()) !==
      JSON.stringify([...(originalContext?.skill_ids ?? [])].sort());
  const thresholdValid = isOptionalFloat(threshold, { min: 0, max: 1 });
  async function save(event: FormEvent) {
    event.preventDefault();
    setError(null);
    const id = behavior?.behavior_id ?? behaviorId;
    try {
      if (!contextId && (systemPrompt || toolsId || compactionId || skillIds.length))
        throw new Error(
          "Choose a context ID to configure prompt, tools, skills, or compaction.",
        );
      if (!compactionId && (threshold || strategy !== "StripThenSummarize"))
        throw new Error("Choose a compaction ID to configure compaction settings.");
      if (!principal) throw new Error("Principal configuration is not available.");
      await onApplyConfigComponents({
        document: {
          agent_principal: { agent_did: agentDid },
          agent_behaviors: [
            {
              ...behavior,
              agent_did: agentDid,
              behavior_id: id,
              display_name: displayName || null,
              context_id: contextId || null,
              inference_profile_id: profileId,
              enabled,
            },
          ],
          contexts: contextId
            ? [
                {
                  ...context,
                  agent_did: agentDid,
                  context_id: contextId,
                  system_prompt: systemPrompt || null,
                  tools_id: toolsId || null,
                  compaction_id: compactionId || null,
                  skill_ids: skillIds,
                },
              ]
            : [],
          compactions: compactionId
            ? [
                {
                  ...compaction,
                  agent_did: agentDid,
                  compaction_id: compactionId,
                  strategy,
                  threshold: parseOptionalFloat(threshold),
                },
              ]
            : [],
        },
      });
      const defaultId = defaultForAgent
        ? id
        : principal.default_behavior_id === id
          ? null
          : principal.default_behavior_id;
      if (defaultId !== principal.default_behavior_id)
        await onSaveAgentConfig({
          document: { ...principal, default_behavior_id: defaultId },
        });
      onSaved(id);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }
  const selectedKnown = new Set(
    skills.filter((skill) => skill.agentDid === agentDid).map((skill) => skill.skillId),
  );
  return (
    <form className="panel config-editor behavior-config-editor" onSubmit={save}>
      <ConfigEditorHeader
        eyebrow="Behavior"
        title={displayName || behaviorId || "New behavior"}
        saved={savedStatus === `behavior:${behaviorId}`}
        dirty={dirty}
      />
      {error ? <FieldHint show>{error}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Behavior ID</span>
          <input
            data-testid="behavior-id"
            readOnly={Boolean(behavior)}
            value={behaviorId}
            onChange={(event) => {
              if (!behavior) setBehaviorId(event.currentTarget.value);
            }}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="behavior-display-name"
            value={displayName}
            onChange={(event) => setDisplayName(event.currentTarget.value)}
          />
        </label>
      </div>
      <label className="field">
        <span>Inference profile</span>
        <select
          data-testid="behavior-profile-id"
          value={profileId}
          onChange={(event) => setProfileId(event.currentTarget.value)}
        >
          <option value="">Select a profile</option>
          {profileId && !profile ? (
            <option value={profileId}>{profileId} (unavailable)</option>
          ) : null}
          {inferenceProfiles.map((entry) => (
            <option key={entry.profile_id} value={entry.profile_id}>
              {entry.display_name ?? entry.profile_id}
            </option>
          ))}
        </select>
        <button
          type="button"
          data-testid="behavior-create-profile"
          onClick={onCreateProfile}
        >
          Create inference profile
        </button>
        <span>
          {profile
            ? `${profile.backend_id} · ${profile.model_name}`
            : "Select a configured inference profile."}
        </span>
      </label>
      <label className="field">
        <span>Context ID</span>
        <input
          data-testid="behavior-context-id"
          value={contextId}
          onChange={(event) => loadContext(event.currentTarget.value)}
        />
        <span>
          Reuse a context or explicitly name a new one. Shared contexts affect all
          referencing behaviors.
        </span>
      </label>
      <label className="field">
        <span>Tools</span>
        <select
          data-testid="behavior-tools-id"
          value={toolsId}
          onChange={(event) => setToolsId(event.currentTarget.value)}
        >
          <option value="">No tools</option>
          {toolsId && !tools.some((entry) => entry.tools_id === toolsId) ? (
            <option value={toolsId}>{toolsId} (unavailable)</option>
          ) : null}
          {tools.map((entry) => (
            <option key={entry.tools_id} value={entry.tools_id}>
              {entry.display_name ?? entry.tools_id}
            </option>
          ))}
        </select>
        <button
          type="button"
          data-testid="behavior-create-tools"
          onClick={onCreateTools}
        >
          Create tools
        </button>
      </label>
      <div className="grid-2">
        <label className="checkbox">
          <input
            data-testid="behavior-enabled"
            type="checkbox"
            checked={enabled}
            onChange={(event) => setEnabled(event.currentTarget.checked)}
          />
          <span>Enabled</span>
        </label>
        <label className="checkbox">
          <input
            data-testid="behavior-default-for-agent"
            type="checkbox"
            checked={defaultForAgent}
            onChange={(event) => setDefaultForAgent(event.currentTarget.checked)}
          />
          <span>Default for this principal</span>
        </label>
      </div>
      <label className="field">
        <span>Compaction ID</span>
        <input
          data-testid="behavior-compaction-id"
          value={compactionId}
          onChange={(event) => loadCompaction(event.currentTarget.value)}
        />
      </label>
      <div className="grid-2">
        <label className="field">
          <span>Compaction strategy</span>
          <select
            data-testid="behavior-compaction-strategy"
            value={strategy ?? "StripThenSummarize"}
            onChange={(event) =>
              setStrategy(event.currentTarget.value as CompactionConfig["strategy"])
            }
          >
            <option value="StripThenSummarize">Strip, then summarize</option>
            <option value="StripToolResults">Strip tool results</option>
          </select>
        </label>
        <label className="field">
          <span>Compaction threshold</span>
          <input
            data-testid="behavior-compaction-threshold"
            value={threshold}
            placeholder="Default"
            onChange={(event) => setThreshold(event.currentTarget.value)}
          />
          <FieldHint show={!thresholdValid}>Number between 0 and 1</FieldHint>
        </label>
      </div>
      <BehaviorToolSurface
        api={api}
        agentDid={agentDid}
        behaviorId={behavior?.behavior_id ?? null}
      />
      <section className="behavior-skills-box" data-testid="behavior-skills">
        <h3>Selected skills</h3>
        <p>Only explicitly selected skills are available to this context.</p>
        {skills
          .filter((skill) => skill.agentDid === agentDid)
          .map((skill) => (
            <label className="checkbox" key={skill.skillId}>
              <input
                data-testid={`behavior-skill-ref-${skill.skillId}`}
                type="checkbox"
                checked={skillIds.includes(skill.skillId)}
                onChange={(event) => {
                  const checked = event.currentTarget.checked;
                  setSkillIds((current) =>
                    checked
                      ? [...current.filter((id) => id !== skill.skillId), skill.skillId]
                      : current.filter((id) => id !== skill.skillId),
                  );
                }}
              />
              <span>
                {skill.name ?? skill.displayName ?? skill.skillId}
                {skill.enabled === false ? " (disabled)" : ""}
              </span>
            </label>
          ))}
        {skillIds
          .filter((id) => !selectedKnown.has(id))
          .map((id) => (
            <label className="checkbox" key={id}>
              <input
                type="checkbox"
                checked
                onChange={() =>
                  setSkillIds((current) => current.filter((value) => value !== id))
                }
              />
              <span>{id} (unavailable)</span>
            </label>
          ))}
      </section>
      <label className="field">
        <span>System prompt</span>
        <textarea
          data-testid="behavior-system-prompt"
          rows={18}
          value={systemPrompt}
          onChange={(event) => setSystemPrompt(event.currentTarget.value)}
        />
        <span>Literal context instructions; prompt templates belong to tasks.</span>
      </label>
      <div className="config-actions">
        {behavior ? (
          <button
            className="ghost-button danger-button"
            data-testid="behavior-delete"
            type="button"
            disabled={saving}
            onClick={() => setConfirmDelete(true)}
          >
            Delete behavior
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmDelete}
          title="Delete behavior"
          message="Existing references must be removed before deletion."
          confirmLabel="Delete"
          danger
          onCancel={() => setConfirmDelete(false)}
          onConfirm={() => {
            setConfirmDelete(false);
            if (behavior)
              void onDeleteBehaviorConfig({
                agentDid,
                behaviorId: behavior.behavior_id,
              })
                .then(onDeleted)
                .catch((caught) => setError(String(caught)));
          }}
        />
        <button
          className="primary-button"
          data-testid="behavior-save"
          type="submit"
          disabled={saving || !behaviorId.trim() || !profile || !thresholdValid}
        >
          {saving ? "Saving..." : "Save behavior"}
        </button>
      </div>
    </form>
  );
}
