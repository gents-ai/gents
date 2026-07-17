import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  EventTriggerDeleteRequest,
  EventTriggerSaveRequest,
  EventTriggerView,
  TaskView,
} from "../../lib/types";
import { ConfirmDialog } from "../ConfirmDialog";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { optionalString } from "./formUtils";

export type EventTriggerConfigPanelProps = {
  deployment: DeploymentView;
  selectedEventTriggerId: string | null;
  selectedTaskId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectEventTrigger: (triggerId: string) => void;
  onCreateEventTrigger: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveEventTriggerConfig: (request: EventTriggerSaveRequest) => Promise<unknown>;
  onDeleteEventTriggerConfig: (request: EventTriggerDeleteRequest) => Promise<unknown>;
  onDeletedEventTrigger: () => void;
};

export function EventTriggerConfigPanel({
  deployment,
  selectedEventTriggerId,
  selectedTaskId,
  saving,
  savedStatus,
  onSelectEventTrigger,
  onCreateEventTrigger,
  onSavedStatusChange,
  onSaveEventTriggerConfig,
  onDeleteEventTriggerConfig,
  onDeletedEventTrigger,
}: EventTriggerConfigPanelProps) {
  const selectedEventTrigger = useMemo(
    () =>
      deployment.eventTriggers.find(
        (trigger) => trigger.triggerId === selectedEventTriggerId,
      ) ?? null,
    [deployment.eventTriggers, selectedEventTriggerId],
  );
  const selectedTask = useMemo(
    () => deployment.tasks.find((task) => task.taskId === selectedTaskId) ?? null,
    [deployment.tasks, selectedTaskId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Triggers"
        items={deployment.eventTriggers.map((trigger) => ({
          id: trigger.triggerId,
          title: trigger.triggerId,
          meta: `${trigger.sourceCollection ?? "collection"} / ${trigger.taskId ?? "no task"}`,
        }))}
        selectedId={selectedEventTriggerId}
        testPrefix="event-trigger"
        title="Event Triggers"
        onCreate={onCreateEventTrigger}
        onSelect={onSelectEventTrigger}
      />

      <EventTriggerConfigEditor
        agentDid={deployment.agentDid}
        eventTrigger={selectedEventTrigger}
        savedStatus={savedStatus}
        saving={saving}
        selectedTask={selectedTask}
        tasks={deployment.tasks}
        onSaved={(triggerId) => {
          onSelectEventTrigger(triggerId);
          onSavedStatusChange(`event-trigger:${triggerId}`);
        }}
        onSaveEventTriggerConfig={onSaveEventTriggerConfig}
        onDeleteEventTriggerConfig={onDeleteEventTriggerConfig}
        onDeleted={() => {
          onDeletedEventTrigger();
        }}
      />
    </section>
  );
}

export type EventTriggerConfigEditorProps = {
  agentDid: string;
  eventTrigger: EventTriggerView | null;
  selectedTask: TaskView | null;
  tasks: TaskView[];
  savedStatus: string | null;
  saving: boolean;
  onSaved: (triggerId: string) => void;
  onSaveEventTriggerConfig: (request: EventTriggerSaveRequest) => Promise<unknown>;
  onDeleteEventTriggerConfig: (request: EventTriggerDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
};

export function EventTriggerConfigEditor({
  agentDid,
  eventTrigger,
  selectedTask,
  tasks,
  savedStatus,
  saving,
  onSaved,
  onSaveEventTriggerConfig,
  onDeleteEventTriggerConfig,
  onDeleted,
}: EventTriggerConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteEventTrigger() {
    setConfirmingDelete(false);
    if (!eventTrigger) {
      return;
    }
    try {
      await onDeleteEventTriggerConfig({
        triggerId: eventTrigger.triggerId,
        agentDid,
      });
      onDeleted();
    } catch {
      // Surfaced by the shell error banner; the editor stays put.
    }
  }
  const [triggerId, setTriggerId] = useState("");
  const [taskId, setTaskId] = useState("");
  const [sourceCollection, setSourceCollection] = useState("AgentRequest");
  const [eventKind, setEventKind] = useState("created");
  const [filter, setFilter] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [concurrency, setConcurrency] = useState("serial");

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const b = eventTriggerFormValues(eventTrigger, selectedTask?.taskId ?? null);
    setTriggerId(b.triggerId);
    setTaskId(b.taskId);
    setSourceCollection(b.sourceCollection);
    setEventKind(b.eventKind);
    setFilter(b.filter);
    setEnabled(b.enabled);
    setConcurrency(b.concurrency);
    setSaveError(null);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
  }, [eventTrigger?.triggerId, selectedTask?.taskId]);

  async function submitEventTrigger(event: FormEvent) {
    event.preventDefault();
    const nextId = triggerId.trim();
    try {
      await onSaveEventTriggerConfig({
        triggerId: nextId,
        taskId,
        sourceCollection,
        eventKind,
        filter: optionalString(filter),
        enabled,
        concurrency,
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitEventTrigger}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            triggerId,
            taskId,
            sourceCollection,
            eventKind,
            filter,
            enabled,
            concurrency,
          },
          eventTriggerFormValues(eventTrigger, selectedTask?.taskId ?? null),
        )}
        eyebrow="Event Trigger"
        saved={savedStatus === `event-trigger:${triggerId.trim()}`}
        title={triggerId || "New Event Trigger"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Trigger ID</span>
          <input
            data-testid="event-trigger-id"
            onChange={(event) => {
              if (!eventTrigger) {
                setTriggerId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(eventTrigger)}
            title={
              eventTrigger
                ? "Event trigger IDs cannot be renamed after creation."
                : undefined
            }
            value={triggerId}
          />
        </label>
        <label className="field">
          <span>Task</span>
          <select
            data-testid="event-trigger-task-id"
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
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Source collection</span>
          <input
            data-testid="event-trigger-source-collection"
            onChange={(event) => setSourceCollection(event.currentTarget.value)}
            value={sourceCollection}
          />
        </label>
        <label className="field">
          <span>Event kind</span>
          <select
            data-testid="event-trigger-event-kind"
            onChange={(event) => setEventKind(event.currentTarget.value)}
            value={eventKind}
          >
            <option value="created">Created</option>
          </select>
        </label>
        <label className="field">
          <span>Concurrency</span>
          <select
            data-testid="event-trigger-concurrency"
            onChange={(event) => setConcurrency(event.currentTarget.value)}
            value={concurrency}
          >
            <option value="serial">Serial</option>
            <option value="parallel">Parallel</option>
            <option value="latest_only">Latest only</option>
          </select>
        </label>
      </div>
      <label className="field">
        <span>Filter</span>
        <textarea
          className="config-small-textarea"
          data-testid="event-trigger-filter"
          onChange={(event) => setFilter(event.currentTarget.value)}
          value={filter}
        />
      </label>
      <label className="checkbox">
        <input
          checked={enabled}
          data-testid="event-trigger-enabled"
          onChange={(event) => setEnabled(event.currentTarget.checked)}
          type="checkbox"
        />
        <span>Enabled</span>
      </label>
      <div className="facts">
        <div>
          <dt>Last status</dt>
          <dd>{eventTrigger?.lastStatus ?? "none"}</dd>
        </div>
        <div>
          <dt>Fire count</dt>
          <dd>{eventTrigger?.fireCount ?? 0}</dd>
        </div>
        <div>
          <dt>Last attempt</dt>
          <dd>{eventTrigger?.lastAttemptAt ?? "none"}</dd>
        </div>
        <div>
          <dt>Last source doc</dt>
          <dd>{eventTrigger?.lastFiredSourceDocId ?? "none"}</dd>
        </div>
        <div>
          <dt>Last error</dt>
          <dd>{eventTrigger?.lastError ?? "none"}</dd>
        </div>
      </div>
      <div className="config-actions">
        {eventTrigger ? (
          <button
            className="ghost-button danger-button"
            data-testid="event-trigger-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete EventTrigger
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete event-trigger"
          message={`Delete event trigger "${eventTrigger?.triggerId ?? ""}"? This automation stops firing immediately.`}
          confirmLabel="Delete EventTrigger"
          danger
          onConfirm={() => {
            void deleteEventTrigger();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="event-trigger-save"
          disabled={
            saving ||
            !triggerId.trim() ||
            !taskId.trim() ||
            !sourceCollection.trim() ||
            !eventKind.trim()
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Event Trigger"}
        </button>
      </div>
    </form>
  );
}

/** View→form hydration, shared by the reset effect and dirty comparison. */
function eventTriggerFormValues(
  eventTrigger: EventTriggerView | null,
  fallbackTaskId: string | null,
) {
  return {
    triggerId: eventTrigger?.triggerId ?? "",
    taskId: eventTrigger?.taskId ?? fallbackTaskId ?? "",
    sourceCollection: eventTrigger?.sourceCollection ?? "AgentRequest",
    eventKind: "created",
    filter: eventTrigger?.filter ?? "",
    enabled: eventTrigger?.enabled ?? true,
    concurrency: eventTrigger?.concurrency ?? "serial",
  };
}
