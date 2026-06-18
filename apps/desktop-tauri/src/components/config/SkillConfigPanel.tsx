import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  SkillDeleteRequest,
  SkillSaveRequest,
  SkillView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
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

  useEffect(() => {
    setSkillId(skill?.skillId ?? "");
    setName(skill?.name ?? skill?.skillId ?? "");
    setScope(skill?.scope ?? "behavior");
    setDescription(skill?.description ?? "");
    setInstructions(skill?.instructions ?? "");
    setToolRefs((skill?.toolRefs ?? []).join("\n"));
    setDisplayName(skill?.displayName ?? "");
    setEnabled(skill?.enabled ?? true);
  }, [skill]);

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
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  async function deleteSkill() {
    const nextId = skill?.skillId ?? skillId.trim();
    if (!skill || !nextId) {
      return;
    }
    const confirmed = window.confirm(
      `Delete skill "${nextId}" and remove it from behavior bindings?`,
    );
    if (!confirmed) {
      return;
    }
    try {
      await onDeleteSkillConfig({ skillId: nextId, agentDid });
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
      />
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
            onClick={deleteSkill}
            type="button"
          >
            Delete Skill
          </button>
        ) : null}
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
