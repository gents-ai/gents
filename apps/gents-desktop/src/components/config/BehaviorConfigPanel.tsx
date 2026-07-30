import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  AgentConfigSaveRequest,
  BehaviorDeleteRequest,
  BehaviorSaveRequest,
  BehaviorView,
  DeploymentView,
  DesktopApiAdapter,
  InferenceBackendView,
  InferenceProfileView,
  SkillView,
  ToolSelectionView,
} from "@source-inc/gents-desktop-client";
import { BehaviorToolSurface } from "./BehaviorToolSurface";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import {
  ConfigDocumentList,
  EditorStatusChip,
  FieldHint,
  PlusIcon,
} from "./ConfigChrome";
import {
  boolText,
  isOptionalFloat,
  optionalString,
  parseOptionalFloat,
} from "./formUtils";

const DEFAULT_COMPACTION_STRATEGY = "StripThenSummarize";
const DEFAULT_COMPACTION_THRESHOLD = "0.75";

export type BehaviorConfigPanelProps = {
  api?: DesktopApiAdapter;
  deployment: DeploymentView;
  selectedBehavior: BehaviorView | null;
  saving: boolean;
  savedStatus: string | null;
  onSavedStatusChange: (value: string) => void;
  onCreateBehavior: () => void;
  onCreateBackend: () => void;
  onCreateProfile: () => void;
  onCreateToolSelection: () => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
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
  onCreateBackend,
  onCreateProfile,
  onCreateToolSelection,
  onSelectBehavior,
  onSaveAgentConfig,
  onSaveBehaviorConfig,
  onDeleteBehaviorConfig,
  onDeletedBehavior,
}: BehaviorConfigPanelProps) {
  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Documents"
        items={deployment.behaviors.map((behavior) => ({
          id: behavior.behaviorId,
          title: behavior.behaviorId,
          meta: behavior.isDefault ? "default" : boolText(behavior.enabled),
        }))}
        selectedId={selectedBehavior?.behaviorId ?? null}
        testPrefix="behavior"
        title="Agent Behaviors"
        onCreate={onCreateBehavior}
        onSelect={onSelectBehavior}
      />

      <BehaviorConfigEditor
        api={api}
        agentDisplayName={
          deployment.agentPrincipal.displayName ?? deployment.label ?? "Agent"
        }
        agentDid={deployment.agentDid}
        agentEnabled={deployment.agentPrincipal.enabled ?? true}
        behavior={selectedBehavior}
        currentDefaultBehaviorId={
          deployment.defaultBehaviorId ??
          deployment.agentPrincipal.defaultBehaviorId ??
          null
        }
        inferenceBackends={deployment.inferenceBackends}
        inferenceProfiles={deployment.inferenceProfiles}
        savedStatus={savedStatus}
        saving={saving}
        skills={deployment.skills}
        toolSelections={deployment.toolSelections}
        onCreateBackend={onCreateBackend}
        onCreateProfile={onCreateProfile}
        onCreateToolSelection={onCreateToolSelection}
        onSaved={(behaviorId) => {
          onSelectBehavior(behaviorId);
          onSavedStatusChange(`behavior:${behaviorId}`);
        }}
        onSaveAgentConfig={onSaveAgentConfig}
        onSaveBehaviorConfig={onSaveBehaviorConfig}
        onDeleteBehaviorConfig={onDeleteBehaviorConfig}
        onDeleted={() => {
          onDeletedBehavior();
        }}
      />
    </section>
  );
}

export type BehaviorConfigEditorProps = {
  api?: DesktopApiAdapter;
  agentDisplayName: string;
  agentDid: string;
  agentEnabled: boolean;
  behavior: BehaviorView | null;
  currentDefaultBehaviorId: string | null;
  inferenceBackends: InferenceBackendView[];
  inferenceProfiles: InferenceProfileView[];
  skills: SkillView[];
  toolSelections: ToolSelectionView[];
  saving: boolean;
  savedStatus: string | null;
  onCreateBackend: () => void;
  onCreateProfile: () => void;
  onCreateToolSelection: () => void;
  onSaved: (behaviorId: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onDeleteBehaviorConfig: (request: BehaviorDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
};

export function BehaviorConfigEditor({
  api,
  agentDisplayName,
  agentDid,
  agentEnabled,
  behavior,
  currentDefaultBehaviorId,
  inferenceBackends,
  inferenceProfiles,
  skills = [],
  toolSelections,
  saving,
  savedStatus,
  onCreateBackend,
  onCreateProfile,
  onCreateToolSelection,
  onSaved,
  onSaveAgentConfig,
  onSaveBehaviorConfig,
  onDeleteBehaviorConfig,
  onDeleted,
}: BehaviorConfigEditorProps) {
  const [behaviorId, setBehaviorId] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [backendId, setBackendId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [toolSelectionId, setToolSelectionId] = useState("");
  const [compactionStrategy, setCompactionStrategy] = useState(
    DEFAULT_COMPACTION_STRATEGY,
  );
  const [compactionThreshold, setCompactionThreshold] = useState(
    DEFAULT_COMPACTION_THRESHOLD,
  );
  const [enabled, setEnabled] = useState(true);
  const [defaultForAgent, setDefaultForAgent] = useState(false);
  const [skillRefs, setSkillRefs] = useState<string[]>([]);
  const [skillExcludes, setSkillExcludes] = useState<string[]>([]);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteBehavior() {
    setConfirmingDelete(false);
    if (!behavior) {
      return;
    }
    try {
      await onDeleteBehaviorConfig({ behaviorId: behavior.behaviorId, agentDid });
      onDeleted();
    } catch {}
  }

  useEffect(() => {
    const base = behaviorFormValues(behavior);
    setBehaviorId(base.behaviorId);
    setSystemPrompt(base.systemPrompt);
    setBackendId(base.backendId);
    setProfileId(base.profileId);
    setToolSelectionId(base.toolSelectionId);
    setCompactionStrategy(base.compactionStrategy);
    setCompactionThreshold(base.compactionThreshold);
    setEnabled(base.enabled);
    setDefaultForAgent(base.defaultForAgent);
    setSkillRefs(base.skillRefs);
    setSkillExcludes(base.skillExcludes);
    setSaveError(null);
  }, [behavior?.behaviorId]);

  const effectiveProfileId = pickValidProfile(
    [profileId, behavior?.inferenceProfileId ?? ""],
    inferenceProfiles,
  );
  const submittedProfileId =
    profileId || behavior?.inferenceProfileId || effectiveProfileId;

  const behaviorScopedSkills = skills.filter((skill) => skill.scope === "behavior");
  const principalScopedSkills = skills.filter((skill) => skill.scope === "principal");

  function toggleSkillRef(skillId: string, checked: boolean) {
    setSkillRefs((current) =>
      checked
        ? Array.from(new Set([...current, skillId]))
        : current.filter((id) => id !== skillId),
    );
  }

  function toggleSkillExclude(skillId: string, excluded: boolean) {
    setSkillExcludes((current) =>
      excluded
        ? Array.from(new Set([...current, skillId]))
        : current.filter((id) => id !== skillId),
    );
  }

  const selectedBackend = inferenceBackends.find(
    (backend) => backend.backendId === backendId,
  );
  const backendModels = selectedBackend?.models.filter(Boolean) ?? [];
  const resolvedModel = backendModels.length
    ? backendModels.join(", ")
    : selectedBackend
      ? "No models configured"
      : "Select a backend";
  const promptRows = Math.min(
    28,
    Math.max(14, systemPrompt.split("\n").length + Math.ceil(systemPrompt.length / 90)),
  );
  const compactionThresholdValid = isOptionalFloat(compactionThreshold, {
    min: 0,
    max: 1,
  });

  const base = behaviorFormValues(behavior);
  const dirty = isDirty(
    {
      behaviorId,
      systemPrompt,
      backendId,
      profileId: effectiveProfileId,
      toolSelectionId,
      compactionStrategy,
      compactionThreshold,
      enabled,
      defaultForAgent,
      skillRefs: [...skillRefs].sort(),
      skillExcludes: [...skillExcludes].sort(),
    },
    {
      ...base,
      profileId: pickValidProfile([base.profileId], inferenceProfiles),
      skillRefs: [...base.skillRefs].sort(),
      skillExcludes: [...base.skillExcludes].sort(),
    },
  );

  async function submitBehavior(event: FormEvent) {
    event.preventDefault();
    const nextId = behaviorId.trim();
    try {
      await onSaveBehaviorConfig({
        agentDid,
        behaviorId: nextId,
        displayName: nextId,
        systemPrompt,
        backendId: optionalString(backendId),
        inferenceProfileId: submittedProfileId,
        toolSelectionId: optionalString(toolSelectionId),
        compactionStrategy: optionalString(compactionStrategy),
        compactionThreshold: parseOptionalFloat(compactionThreshold),
        enabled,
        skillRefs,
        skillExcludes,
      });
      if (defaultForAgent && nextId !== currentDefaultBehaviorId) {
        await onSaveAgentConfig({
          agentDid,
          displayName: agentDisplayName,
          defaultBehaviorId: nextId,
          enabled: agentEnabled,
        });
      }
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form
      className="panel config-editor behavior-config-editor"
      onSubmit={submitBehavior}
    >
      <div className="panel-header behavior-config-header">
        <div>
          <p className="eyebrow">Behavior</p>
          <div className="behavior-key-row">
            {!behavior ? (
              <input
                className="behavior-key-input"
                data-testid="behavior-id"
                onChange={(event) => setBehaviorId(event.currentTarget.value)}
                value={behaviorId}
              />
            ) : (
              <h3>{behaviorId || "New Behavior"}</h3>
            )}
          </div>
        </div>
        <EditorStatusChip
          dirty={dirty}
          saved={savedStatus === `behavior:${behaviorId.trim()}`}
        />
      </div>
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}

      <div className="behavior-link-grid">
        <label className="field behavior-link-field">
          <span>Backend</span>
          <div className="behavior-link-control">
            <select
              data-testid="behavior-backend-id"
              onChange={(event) => setBackendId(event.currentTarget.value)}
              value={backendId}
            >
              <option value="">Unset</option>
              {inferenceBackends.map((backend) => (
                <option key={backend.backendId} value={backend.backendId}>
                  {backend.name ?? backend.backendId}
                </option>
              ))}
            </select>
            <button
              aria-label="Create backend"
              className="ghost-button config-icon-button"
              data-testid="behavior-create-backend"
              onClick={onCreateBackend}
              title="Create backend"
              type="button"
            >
              <PlusIcon />
            </button>
          </div>
        </label>
        <label className="field behavior-link-field">
          <span>Profile</span>
          <div className="behavior-link-control">
            <select
              data-testid="behavior-profile-id"
              onChange={(event) => setProfileId(event.currentTarget.value)}
              value={effectiveProfileId}
            >
              {!inferenceProfiles.length ? (
                <option value="">No profiles available</option>
              ) : null}
              {inferenceProfiles.map((profile) => (
                <option key={profile.profileId} value={profile.profileId}>
                  {profile.displayName ?? profile.profileId}
                </option>
              ))}
            </select>
            <button
              aria-label="Create inference profile"
              className="ghost-button config-icon-button"
              data-testid="behavior-create-profile"
              onClick={onCreateProfile}
              title="Create inference profile"
              type="button"
            >
              <PlusIcon />
            </button>
          </div>
        </label>
        <label className="field behavior-link-field">
          <span>Tool selection</span>
          <div className="behavior-link-control">
            <select
              data-testid="behavior-tool-selection-id"
              onChange={(event) => setToolSelectionId(event.currentTarget.value)}
              value={toolSelectionId}
            >
              <option value="">Unset</option>
              {toolSelections.map((selection) => (
                <option key={selection.selectionId} value={selection.selectionId}>
                  {selection.displayName ?? selection.selectionId}
                </option>
              ))}
            </select>
            <button
              aria-label="Create tool selection"
              className="ghost-button config-icon-button"
              data-testid="behavior-create-tool-selection"
              onClick={onCreateToolSelection}
              title="Create tool selection"
              type="button"
            >
              <PlusIcon />
            </button>
          </div>
        </label>
      </div>

      <div className="grid-2 behavior-state-grid">
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="behavior-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
        <label className="checkbox">
          <input
            checked={defaultForAgent}
            data-testid="behavior-default-for-agent"
            disabled={behavior?.isDefault ?? false}
            onChange={(event) => setDefaultForAgent(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Default behavior</span>
        </label>
      </div>

      <section className="behavior-compaction-box">
        <div className="grid-2">
          <label className="field">
            <span>Strategy</span>
            <select
              data-testid="behavior-compaction-strategy"
              onChange={(event) => setCompactionStrategy(event.currentTarget.value)}
              value={compactionStrategy}
            >
              <option value="StripThenSummarize">Strip then summarize</option>
              <option value="StripToolResults">Strip tool results</option>
              <option value="Summarize">Summarize</option>
            </select>
          </label>
          <label className="field">
            <span>Threshold</span>
            <input
              data-testid="behavior-compaction-threshold"
              max="1"
              min="0"
              onChange={(event) => setCompactionThreshold(event.currentTarget.value)}
              step="0.01"
              type="number"
              value={compactionThreshold}
            />
            <FieldHint show={!compactionThresholdValid}>
              Number between 0 and 1
            </FieldHint>
          </label>
        </div>
      </section>

      <div className="facts facts-single">
        <div>
          <dt>Backend model</dt>
          <dd>{resolvedModel}</dd>
        </div>
      </div>

      <BehaviorToolSurface
        agentDid={agentDid}
        api={api}
        behaviorId={behavior?.behaviorId ?? null}
      />

      <section className="behavior-skills-box" data-testid="behavior-skills">
        <p className="eyebrow">Skills</p>
        {!skills.length ? (
          <p className="muted">
            No skills defined for this agent. Create skills in the Skills tab.
          </p>
        ) : null}
        {behaviorScopedSkills.length ? (
          <fieldset className="behavior-skill-group">
            <legend>Behavior-scoped (opt in)</legend>
            {behaviorScopedSkills.map((skill) => (
              <label className="checkbox" key={skill.skillId}>
                <input
                  checked={skillRefs.includes(skill.skillId)}
                  data-testid={`behavior-skill-ref-${skill.skillId}`}
                  onChange={(event) =>
                    toggleSkillRef(skill.skillId, event.currentTarget.checked)
                  }
                  type="checkbox"
                />
                <span>{skill.displayName ?? skill.name ?? skill.skillId}</span>
              </label>
            ))}
          </fieldset>
        ) : null}
        {principalScopedSkills.length ? (
          <fieldset className="behavior-skill-group">
            <legend>Principal-scoped (inherited; uncheck to exclude)</legend>
            {principalScopedSkills.map((skill) => (
              <label className="checkbox" key={skill.skillId}>
                <input
                  checked={!skillExcludes.includes(skill.skillId)}
                  data-testid={`behavior-skill-inherit-${skill.skillId}`}
                  onChange={(event) =>
                    toggleSkillExclude(skill.skillId, !event.currentTarget.checked)
                  }
                  type="checkbox"
                />
                <span>{skill.displayName ?? skill.name ?? skill.skillId}</span>
              </label>
            ))}
          </fieldset>
        ) : null}
      </section>

      <label className="field">
        <span>System prompt</span>
        <textarea
          className="config-textarea behavior-system-prompt"
          data-testid="behavior-system-prompt"
          onChange={(event) => setSystemPrompt(event.currentTarget.value)}
          rows={promptRows}
          value={systemPrompt}
        />
      </label>

      <div className="config-actions">
        {behavior && !behavior.isDefault ? (
          <button
            className="ghost-button danger-button"
            data-testid="behavior-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Behavior
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete behavior"
          message={`Delete behavior "${behavior?.behaviorId ?? ""}"? Tasks still pointing at it will block the delete.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            void deleteBehavior();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="behavior-save"
          disabled={
            saving ||
            !behaviorId.trim() ||
            !systemPrompt.trim() ||
            !effectiveProfileId ||
            !compactionThresholdValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Behavior"}
        </button>
      </div>
    </form>
  );
}

function behaviorFormValues(behavior: BehaviorView | null) {
  return {
    behaviorId: behavior?.behaviorId ?? "",
    systemPrompt: behavior?.systemPrompt ?? "",
    backendId: behavior?.backendId ?? "",
    profileId: behavior?.inferenceProfileId ?? "",
    toolSelectionId: behavior?.toolSelectionId ?? "",
    compactionStrategy: behavior?.compactionStrategy ?? DEFAULT_COMPACTION_STRATEGY,
    compactionThreshold:
      behavior?.compactionThreshold != null
        ? String(behavior.compactionThreshold)
        : DEFAULT_COMPACTION_THRESHOLD,
    enabled: behavior?.enabled ?? true,
    defaultForAgent: behavior?.isDefault ?? false,
    skillRefs: behavior?.skillRefs ?? [],
    skillExcludes: behavior?.skillExcludes ?? [],
  };
}

function pickValidProfile(candidates: string[], profiles: InferenceProfileView[]) {
  for (const candidate of candidates) {
    if (candidate && profiles.some((profile) => profile.profileId === candidate)) {
      return candidate;
    }
  }
  return profiles[0]?.profileId ?? "";
}
