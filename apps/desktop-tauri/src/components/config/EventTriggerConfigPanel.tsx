import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  EventTriggerSaveRequest,
  EventTriggerView,
  TaskView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
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
  onSaveEventTriggerConfig: (
    request: EventTriggerSaveRequest,
  ) => Promise<unknown>;
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
      />
    </section>
  );
}

export type EventTriggerConfigEditorProps = {
  eventTrigger: EventTriggerView | null;
  selectedTask: TaskView | null;
  tasks: TaskView[];
  savedStatus: string | null;
  saving: boolean;
  onSaved: (triggerId: string) => void;
  onSaveEventTriggerConfig: (
    request: EventTriggerSaveRequest,
  ) => Promise<unknown>;
};

export function EventTriggerConfigEditor({
  eventTrigger,
  selectedTask,
  tasks,
  savedStatus,
  saving,
  onSaved,
  onSaveEventTriggerConfig,
}: EventTriggerConfigEditorProps) {
  const [triggerId, setTriggerId] = useState("");
  const [taskId, setTaskId] = useState("");
  const [sourceCollection, setSourceCollection] = useState("AgentRequest");
  const [eventKind, setEventKind] = useState("created");
  const [filter, setFilter] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [concurrency, setConcurrency] = useState("serial");

  useEffect(() => {
    setTriggerId(eventTrigger?.triggerId ?? "");
    setTaskId(eventTrigger?.taskId ?? selectedTask?.taskId ?? "");
    setSourceCollection(eventTrigger?.sourceCollection ?? "AgentRequest");
    setEventKind("created");
    setFilter(eventTrigger?.filter ?? "");
    setEnabled(eventTrigger?.enabled ?? true);
    setConcurrency(eventTrigger?.concurrency ?? "serial");
  }, [eventTrigger, selectedTask?.taskId]);

  async function submitEventTrigger(event: FormEvent) {
    event.preventDefault();
    const nextId = triggerId.trim();
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
  }

  return (
    <form className="panel config-editor" onSubmit={submitEventTrigger}>
      <ConfigEditorHeader
        eyebrow="Event Trigger"
        saved={savedStatus === `event-trigger:${triggerId.trim()}`}
        title={triggerId || "New Event Trigger"}
      />
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
