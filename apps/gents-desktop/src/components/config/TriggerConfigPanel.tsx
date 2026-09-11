import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  Trigger,
  TriggerDeleteRequest,
  TriggerSaveRequest,
  TriggerView,
} from "@source-inc/gents-desktop-client";
import type { ConcurrencyMode } from "@source-inc/gents-desktop-client/generated/ConcurrencyMode";
import type { TriggerSource } from "@source-inc/gents-desktop-client/generated/TriggerSource";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { linesToArray } from "./formUtils";

export type TriggerConfigPanelProps = {
  deployment: DeploymentView;
  selectedTriggerId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectTrigger: (triggerId: string) => void;
  onCreateTrigger: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveTriggerConfig: (request: TriggerSaveRequest) => Promise<unknown>;
  onDeleteTriggerConfig: (request: TriggerDeleteRequest) => Promise<unknown>;
  onDeletedTrigger: () => void;
};

export function TriggerConfigPanel({
  deployment,
  selectedTriggerId,
  saving,
  savedStatus,
  onSelectTrigger,
  onCreateTrigger,
  onSavedStatusChange,
  onSaveTriggerConfig,
  onDeleteTriggerConfig,
  onDeletedTrigger,
}: TriggerConfigPanelProps) {
  const selectedTrigger = useMemo(
    () =>
      deployment.triggers.find(
        (trigger) => trigger.config.trigger_id === selectedTriggerId,
      ) ?? null,
    [deployment.triggers, selectedTriggerId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Automation"
        items={deployment.triggers.map((trigger) => ({
          id: trigger.config.trigger_id,
          title: trigger.config.display_name ?? trigger.config.trigger_id,
          meta: describeSource(trigger),
        }))}
        selectedId={selectedTriggerId}
        testPrefix="trigger"
        title="Triggers"
        onCreate={onCreateTrigger}
        onSelect={onSelectTrigger}
      />

      <TriggerConfigEditor
        agentDid={deployment.agentDid}
        schedules={deployment.schedules}
        eventSources={deployment.eventSources}
        tasks={deployment.tasks}
        trigger={selectedTrigger}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(triggerId) => {
          onSelectTrigger(triggerId);
          onSavedStatusChange(`trigger:${triggerId}`);
        }}
        onSaveTriggerConfig={onSaveTriggerConfig}
        onDeleteTriggerConfig={onDeleteTriggerConfig}
        onDeleted={() => {
          onDeletedTrigger();
        }}
      />
    </section>
  );
}

export type TriggerConfigEditorProps = {
  agentDid: string;
  schedules: DeploymentView["schedules"];
  eventSources: DeploymentView["eventSources"];
  tasks: DeploymentView["tasks"];
  trigger: TriggerView | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (triggerId: string) => void;
  onSaveTriggerConfig: (request: TriggerSaveRequest) => Promise<unknown>;
  onDeleteTriggerConfig: (request: TriggerDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
};

export function TriggerConfigEditor({
  agentDid,
  schedules,
  eventSources,
  tasks,
  trigger,
  savedStatus,
  saving,
  onSaved,
  onSaveTriggerConfig,
  onDeleteTriggerConfig,
  onDeleted,
}: TriggerConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteTrigger() {
    setConfirmingDelete(false);
    if (!trigger) {
      return;
    }
    try {
      await onDeleteTriggerConfig({
        triggerId: trigger.config.trigger_id,
        agentDid,
      });
      onDeleted();
    } catch {}
  }

  const [triggerId, setTriggerId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [taskId, setTaskId] = useState("");
  const [sourceKind, setSourceKind] = useState<"schedule" | "event">("schedule");
  const [scheduleId, setScheduleId] = useState("");
  const [eventSourceId, setEventSourceId] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [concurrency, setConcurrency] = useState<ConcurrencyMode | "">("");
  const [tags, setTags] = useState("");

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const baseline = triggerFormValues(trigger);
    setTriggerId(baseline.triggerId);
    setDisplayName(baseline.displayName);
    setDescription(baseline.description);
    setTaskId(baseline.taskId);
    setSourceKind(baseline.sourceKind);
    setScheduleId(baseline.scheduleId);
    setEventSourceId(baseline.eventSourceId);
    setEnabled(baseline.enabled);
    setConcurrency(baseline.concurrency);
    setTags(baseline.tags);
    setSaveError(null);
  }, [trigger?.config.trigger_id]);

  const sourceValid =
    sourceKind === "schedule"
      ? scheduleId.trim().length > 0
      : eventSourceId.trim().length > 0;

  async function submitTrigger(event: FormEvent) {
    event.preventDefault();
    if (!triggerId.trim() || !taskId.trim() || !sourceValid) {
      return;
    }
    const source: TriggerSource =
      sourceKind === "schedule"
        ? { kind: "schedule", schedule_id: scheduleId.trim() }
        : { kind: "event", event_source_id: eventSourceId.trim() };
    const document: Trigger = {
      agent_did: agentDid,
      trigger_id: triggerId.trim(),
      display_name: optionalTrimmed(displayName),
      description: optionalTrimmed(description),
      task_id: taskId.trim(),
      source,
      enabled,
      concurrency: concurrency === "" ? null : concurrency,
      tags: linesToArray(tags),
    };
    try {
      await onSaveTriggerConfig({ document });
      onSaved(document.trigger_id);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitTrigger}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            triggerId,
            displayName,
            description,
            taskId,
            sourceKind,
            scheduleId,
            eventSourceId,
            enabled,
            concurrency,
            tags,
          },
          triggerFormValues(trigger),
        )}
        eyebrow="Trigger"
        saved={savedStatus === `trigger:${triggerId.trim()}`}
        title={displayName || triggerId || "New Trigger"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Trigger ID</span>
          <input
            data-testid="trigger-id"
            onChange={(event) => {
              if (!trigger) {
                setTriggerId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(trigger)}
            title={
              trigger ? "Trigger IDs cannot be renamed after creation." : undefined
            }
            value={triggerId}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="trigger-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>
      <label className="field">
        <span>Description</span>
        <input
          data-testid="trigger-description"
          onChange={(event) => setDescription(event.currentTarget.value)}
          value={description}
        />
      </label>
      <div className="grid-3">
        <label className="field">
          <span>Task</span>
          <select
            data-testid="trigger-task-id"
            onChange={(event) => setTaskId(event.currentTarget.value)}
            value={taskId}
          >
            <option value="">Unset</option>
            {tasks.map((task) => (
              <option key={task.taskId} value={task.taskId}>
                {task.name ?? task.taskId}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Concurrency</span>
          <select
            data-testid="trigger-concurrency"
            onChange={(event) =>
              setConcurrency(event.currentTarget.value as ConcurrencyMode | "")
            }
            value={concurrency}
          >
            <option value="">Default (parallel)</option>
            <option value="parallel">Parallel</option>
            <option value="serial">Serial</option>
            <option value="latest_only">Latest only</option>
          </select>
        </label>
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="trigger-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Source kind</span>
          <select
            data-testid="trigger-source-kind"
            onChange={(event) =>
              setSourceKind(event.currentTarget.value as "schedule" | "event")
            }
            value={sourceKind}
          >
            <option value="schedule">Schedule</option>
            <option value="event">Event source</option>
          </select>
        </label>
        {sourceKind === "schedule" ? (
          <label className="field">
            <span>Schedule</span>
            <select
              data-testid="trigger-source-schedule"
              onChange={(event) => setScheduleId(event.currentTarget.value)}
              value={scheduleId}
            >
              <option value="">Unset</option>
              {schedules.map((schedule) => (
                <option key={schedule.schedule_id} value={schedule.schedule_id}>
                  {schedule.display_name ?? schedule.schedule_id}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <label className="field">
            <span>Event source</span>
            <select
              data-testid="trigger-source-event-source"
              onChange={(event) => setEventSourceId(event.currentTarget.value)}
              value={eventSourceId}
            >
              <option value="">Unset</option>
              {eventSources.map((source) => (
                <option key={source.event_source_id} value={source.event_source_id}>
                  {source.display_name ?? source.event_source_id}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      <label className="field">
        <span>Tags</span>
        <textarea
          className="config-small-textarea"
          data-testid="trigger-tags"
          onChange={(event) => setTags(event.currentTarget.value)}
          placeholder="One tag per line"
          value={tags}
        />
      </label>
      {trigger ? (
        <div className="facts">
          <div>
            <dt>Last status</dt>
            <dd>{trigger.lastStatus ?? "none"}</dd>
          </div>
          <div>
            <dt>Fire count</dt>
            <dd>{trigger.fireCount ?? 0}</dd>
          </div>
          <div>
            <dt>Next run</dt>
            <dd>{trigger.nextRunAt ?? "not scheduled"}</dd>
          </div>
          <div>
            <dt>Last attempt</dt>
            <dd>{trigger.lastAttemptAt ?? "none"}</dd>
          </div>
          <div>
            <dt>Last source doc</dt>
            <dd>{trigger.lastFiredSourceDocId ?? "none"}</dd>
          </div>
          <div>
            <dt>Last error</dt>
            <dd>{trigger.lastError ?? "none"}</dd>
          </div>
        </div>
      ) : null}
      <div className="config-actions">
        {trigger ? (
          <button
            className="ghost-button danger-button"
            data-testid="trigger-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Trigger
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete trigger"
          message={`Delete trigger "${trigger?.config.trigger_id ?? ""}"? This automation stops firing immediately.`}
          confirmLabel="Delete Trigger"
          danger
          onConfirm={() => {
            void deleteTrigger();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="trigger-save"
          disabled={saving || !triggerId.trim() || !taskId.trim() || !sourceValid}
          type="submit"
        >
          {saving ? "Saving..." : "Save Trigger"}
        </button>
      </div>
    </form>
  );
}

function triggerFormValues(trigger: TriggerView | null) {
  const config = trigger?.config ?? null;
  const source = config?.source ?? null;
  return {
    triggerId: config?.trigger_id ?? "",
    displayName: config?.display_name ?? "",
    description: config?.description ?? "",
    taskId: config?.task_id ?? "",
    sourceKind: source?.kind === "event" ? ("event" as const) : ("schedule" as const),
    scheduleId: source?.kind === "schedule" ? source.schedule_id : "",
    eventSourceId: source?.kind === "event" ? source.event_source_id : "",
    enabled: config?.enabled ?? true,
    concurrency: (config?.concurrency ?? "") as ConcurrencyMode | "",
    tags: config?.tags?.length ? config.tags.join("\n") : "",
  };
}

function optionalTrimmed(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function describeSource(trigger: TriggerView): string {
  if (trigger.config.source.kind === "schedule") {
    return `schedule: ${trigger.config.source.schedule_id}`;
  }
  return `event: ${trigger.config.source.event_source_id}`;
}
