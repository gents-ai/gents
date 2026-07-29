import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  SkillDeleteRequest,
  SkillSaveRequest,
  SkillView,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { ignoreHandledActionError, linesToArray, optionalString } from "./formUtils";

export type SkillConfigPanelProps = {
  deployment: DeploymentView;
  selectedSkillId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectSkill: (skillId: string | null) => void;
  onCreateSkill: () => void;
  onDeletedSkill: () => void;
  onSavedStatusChange: (value: string | null) => void;
  onDeleteSkillConfig: (request: SkillDeleteRequest) => Promise<unknown>;
  onSaveSkillConfig: (request: SkillSaveRequest) => Promise<unknown>;
};

export function SkillConfigPanel({
  deployment,
  selectedSkillId,
  saving,
  savedStatus,
  onSelectSkill,
  onCreateSkill,
  onDeletedSkill,
  onSavedStatusChange,
  onDeleteSkillConfig,
  onSaveSkillConfig,
}: SkillConfigPanelProps) {
  const skills = deployment.skills ?? [];
  const selectedSkill =
    skills.find((skill) => skill.skillId === selectedSkillId) ?? null;

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Skills"
        items={skills.map((skill) => {
          const title = displaySkillListTitle(skill);
          return {
            id: skill.skillId,
            title,
            meta: skill.scope ?? "behavior",
          };
        })}
        selectedId={selectedSkillId}
        testPrefix="skill"
        title="Skills"
        onCreate={onCreateSkill}
        onSelect={onSelectSkill}
      />

      <SkillConfigEditor
        agentDid={deployment.agentDid}
        savedStatus={savedStatus}
        saving={saving}
        skill={selectedSkill}
        onSaved={(skillId) => {
          onSelectSkill(skillId);
          onSavedStatusChange(`skill:${skillId}`);
        }}
        onDeleted={() => {
          onSelectSkill(null);
          onSavedStatusChange(null);
          onDeletedSkill();
        }}
        onDeleteSkillConfig={onDeleteSkillConfig}
        onSaveSkillConfig={onSaveSkillConfig}
      />
    </section>
  );
}

export type SkillConfigEditorProps = {
  agentDid: string;
  skill: SkillView | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (skillId: string) => void;
  onDeleted: () => void;
  onDeleteSkillConfig: (request: SkillDeleteRequest) => Promise<unknown>;
  onSaveSkillConfig: (request: SkillSaveRequest) => Promise<unknown>;
};

export function SkillConfigEditor({
  agentDid,
  skill,
  savedStatus,
  saving,
  onSaved,
  onDeleted,
  onDeleteSkillConfig,
  onSaveSkillConfig,
}: SkillConfigEditorProps) {
  const [skillId, setSkillId] = useState("");
  const [name, setName] = useState("");
  const [scope, setScope] = useState("behavior");
  const [description, setDescription] = useState("");
  const [instructions, setInstructions] = useState("");
  const [toolRefs, setToolRefs] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const base = skillFormValues(skill);
    setSkillId(base.skillId);
    setName(base.name);
    setScope(base.scope);
    setDescription(base.description);
    setInstructions(base.instructions);
    setToolRefs(base.toolRefs);
    setDisplayName(base.displayName);
    setEnabled(base.enabled);
    setSaveError(null);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [skill?.skillId]);

  const dirty = isDirty(
    { skillId, name, scope, description, instructions, toolRefs, displayName, enabled },
    skillFormValues(skill),
  );

  async function submitSkill(event: FormEvent) {
    event.preventDefault();
    const nextId = skillId.trim();
    try {
      await onSaveSkillConfig({
        skillId: nextId,
        agentDid,
        scope,
        name,
        description: optionalString(description),
        instructions,
        toolRefs: linesToArray(toolRefs),
        displayName: optionalString(displayName),
        enabled,
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  function requestDeleteSkill() {
    const nextId = skill?.skillId ?? skillId.trim();
    if (!skill || !nextId) {
      return;
    }
    setConfirmingDelete(true);
  }

  async function deleteSkill() {
    const nextId = skill?.skillId ?? skillId.trim();
    setConfirmingDelete(false);
    if (!skill || !nextId) {
      return;
    }
    try {
      await onDeleteSkillConfig({
        skillId: nextId,
        agentDid: skill.agentDid ?? agentDid,
      });
      onDeleted();
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitSkill}>
      <ConfigEditorHeader
        eyebrow="Skill"
        saved={savedStatus === `skill:${skillId.trim()}`}
        title={name || skillId || "New Skill"}
        dirty={dirty}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Skill ID</span>
          <input
            data-testid="skill-id"
            onChange={(event) => {
              if (!skill) {
                setSkillId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(skill)}
            title={skill ? "Skill IDs cannot be renamed after creation." : undefined}
            value={skillId}
          />
        </label>
        <label className="field">
          <span>Name</span>
          <input
            data-testid="skill-name"
            onChange={(event) => setName(event.currentTarget.value)}
            value={name}
          />
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Scope</span>
          <select
            data-testid="skill-scope"
            onChange={(event) => setScope(event.currentTarget.value)}
            value={scope}
          >
            <option value="behavior">behavior (opt-in per behavior)</option>
            <option value="principal">principal (all behaviors)</option>
          </select>
        </label>
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="skill-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
      </div>
      <label className="field">
        <span>Display name</span>
        <input
          data-testid="skill-display-name"
          onChange={(event) => setDisplayName(event.currentTarget.value)}
          value={displayName}
        />
      </label>
      <label className="field">
        <span>Description (shown in the skills catalog)</span>
        <textarea
          className="config-small-textarea"
          data-testid="skill-description"
          onChange={(event) => setDescription(event.currentTarget.value)}
          value={description}
        />
      </label>
      <label className="field">
        <span>Instructions (loaded on demand via load_skill)</span>
        <textarea
          className="config-textarea"
          data-testid="skill-instructions"
          onChange={(event) => setInstructions(event.currentTarget.value)}
          value={instructions}
        />
      </label>
      <label className="field">
        <span>
          Tool dependencies (one per line; intersected with the behavior ceiling)
        </span>
        <textarea
          className="config-small-textarea"
          data-testid="skill-tool-refs"
          onChange={(event) => setToolRefs(event.currentTarget.value)}
          value={toolRefs}
        />
      </label>
      <div className="config-actions">
        {skill ? (
          <button
            className="ghost-button danger-button"
            data-testid="skill-delete"
            disabled={saving}
            onClick={requestDeleteSkill}
            type="button"
          >
            Delete Skill
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete skill"
          message={`Delete skill "${skill?.skillId ?? skillId.trim()}" and remove it from behavior bindings?`}
          confirmLabel="Delete Skill"
          danger
          onConfirm={() => {
            void deleteSkill();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="skill-save"
          disabled={saving || !skillId.trim() || !name.trim() || !instructions.trim()}
          type="submit"
        >
          {saving ? "Saving..." : "Save Skill"}
        </button>
      </div>
    </form>
  );
}

/** View→form hydration, shared by the reset effect and dirty comparison. */
function skillFormValues(skill: SkillView | null) {
  return {
    skillId: skill?.skillId ?? "",
    name: skill?.name ?? skill?.skillId ?? "",
    scope: skill?.scope ?? "behavior",
    description: skill?.description ?? "",
    instructions: skill?.instructions ?? "",
    toolRefs: (skill?.toolRefs ?? []).join("\n"),
    displayName: skill?.displayName ?? "",
    enabled: skill?.enabled ?? true,
  };
}

function displaySkillListTitle(skill: SkillView) {
  const displayName = skill.displayName?.trim();
  if (displayName) {
    return displayName;
  }
  const name = skill.name?.trim();
  if (name) {
    return name;
  }
  return skill.skillId;
}
