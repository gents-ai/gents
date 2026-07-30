import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  ScheduleDeleteRequest,
  ScheduleSaveRequest,
  ScheduleView,
  TaskRunResult,
  TaskView,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { ignoreHandledActionError, isOptionalInt, parseOptionalInt } from "./formUtils";

export type ScheduleConfigPanelProps = {
  deployment: DeploymentView;
  selectedScheduleId: string | null;
  selectedTaskId: string | null;
  saving: boolean;
  runningTask: boolean;
  savedStatus: string | null;
  onSelectSchedule: (scheduleId: string) => void;
  onCreateSchedule: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveScheduleConfig: (request: ScheduleSaveRequest) => Promise<unknown>;
  onDeleteScheduleConfig: (request: ScheduleDeleteRequest) => Promise<unknown>;
  onDeletedSchedule: () => void;
  onRunSchedule: (request: { scheduleId: string }) => Promise<TaskRunResult>;
};

export function ScheduleConfigPanel({
  deployment,
  selectedScheduleId,
  selectedTaskId,
  saving,
  runningTask,
  savedStatus,
  onSelectSchedule,
  onCreateSchedule,
  onSavedStatusChange,
  onSaveScheduleConfig,
  onDeleteScheduleConfig,
  onDeletedSchedule,
  onRunSchedule,
}: ScheduleConfigPanelProps) {
  const selectedSchedule = useMemo(
    () =>
      deployment.schedules.find(
        (schedule) => schedule.scheduleId === selectedScheduleId,
      ) ?? null,
    [deployment.schedules, selectedScheduleId],
  );
  const selectedTask = useMemo(
    () => deployment.tasks.find((task) => task.taskId === selectedTaskId) ?? null,
    [deployment.tasks, selectedTaskId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Triggers"
        items={deployment.schedules.map((schedule) => ({
          id: schedule.scheduleId,
          title: schedule.scheduleId,
          meta: `${schedule.taskId ?? "no task"} / ${schedule.concurrency ?? "serial"}`,
        }))}
        selectedId={selectedScheduleId}
        testPrefix="schedule"
        title="Timer Triggers"
        onCreate={onCreateSchedule}
        onSelect={onSelectSchedule}
      />

      <ScheduleConfigEditor
        agentDid={deployment.agentDid}
        runningTask={runningTask}
        savedStatus={savedStatus}
        saving={saving}
        schedule={selectedSchedule}
        selectedTask={selectedTask}
        tasks={deployment.tasks}
        onRunSchedule={onRunSchedule}
        onSaved={(scheduleId) => {
          onSelectSchedule(scheduleId);
          onSavedStatusChange(`schedule:${scheduleId}`);
        }}
        onSaveScheduleConfig={onSaveScheduleConfig}
        onDeleteScheduleConfig={onDeleteScheduleConfig}
        onDeleted={() => {
          onDeletedSchedule();
        }}
      />
    </section>
  );
}

export type ScheduleConfigEditorProps = {
  agentDid: string;
  schedule: ScheduleView | null;
  selectedTask: TaskView | null;
  tasks: TaskView[];
  savedStatus: string | null;
  saving: boolean;
  runningTask: boolean;
  onSaved: (scheduleId: string) => void;
  onSaveScheduleConfig: (request: ScheduleSaveRequest) => Promise<unknown>;
  onDeleteScheduleConfig: (request: ScheduleDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
  onRunSchedule: (request: { scheduleId: string }) => Promise<TaskRunResult>;
};

export function ScheduleConfigEditor({
  agentDid,
  schedule,
  selectedTask,
  tasks,
  savedStatus,
  saving,
  runningTask,
  onSaved,
  onSaveScheduleConfig,
  onDeleteScheduleConfig,
  onDeleted,
  onRunSchedule,
}: ScheduleConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteSchedule() {
    setConfirmingDelete(false);
    if (!schedule) {
      return;
    }
    try {
      await onDeleteScheduleConfig({ scheduleId: schedule.scheduleId, agentDid });
      onDeleted();
    } catch {
    }
  }
  const [scheduleId, setScheduleId] = useState("");
  const [taskId, setTaskId] = useState("");
  const [intervalSecs, setIntervalSecs] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [concurrency, setConcurrency] = useState("serial");
  const [runStatus, setRunStatus] = useState<TaskRunResult | null>(null);

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const b = scheduleFormValues(schedule, selectedTask?.taskId ?? null);
    setScheduleId(b.scheduleId);
    setTaskId(b.taskId);
    setIntervalSecs(b.intervalSecs);
    setEnabled(b.enabled);
    setConcurrency(b.concurrency);
    setSaveError(null);
  }, [schedule?.scheduleId, selectedTask?.taskId]);

  useEffect(() => {
    setRunStatus(null);
  }, [schedule?.scheduleId]);

  const intervalValid = isOptionalInt(intervalSecs, { min: 1 });

  async function submitSchedule(event: FormEvent) {
    event.preventDefault();
    const nextId = scheduleId.trim();
    try {
      await onSaveScheduleConfig({
        scheduleId: nextId,
        taskId,
        intervalSecs: parseOptionalInt(intervalSecs),
        enabled,
        concurrency,
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  async function runSelectedSchedule() {
    try {
      const result = await onRunSchedule({ scheduleId: scheduleId.trim() });
      setRunStatus(result);
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitSchedule}>
      <ConfigEditorHeader
        dirty={isDirty(
          { scheduleId, taskId, intervalSecs, enabled, concurrency },
          scheduleFormValues(schedule, selectedTask?.taskId ?? null),
        )}
        eyebrow="Timer Trigger"
        saved={savedStatus === `schedule:${scheduleId.trim()}`}
        title={scheduleId || "New Timer Trigger"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Schedule ID</span>
          <input
            data-testid="schedule-id"
            onChange={(event) => {
              if (!schedule) {
                setScheduleId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(schedule)}
            title={
              schedule ? "Schedule IDs cannot be renamed after creation." : undefined
            }
            value={scheduleId}
          />
        </label>
        <label className="field">
          <span>Task</span>
          <select
            data-testid="schedule-task-id"
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
          <span>Interval seconds</span>
          <input
            data-testid="schedule-interval-secs"
            onChange={(event) => setIntervalSecs(event.currentTarget.value)}
            type="number"
            value={intervalSecs}
          />
          <FieldHint show={!intervalValid}>Whole number of 1 or more</FieldHint>
        </label>
        <label className="field">
          <span>Concurrency</span>
          <select
            data-testid="schedule-concurrency"
            onChange={(event) => setConcurrency(event.currentTarget.value)}
            value={concurrency}
          >
            <option value="serial">Serial</option>
            <option value="parallel">Parallel</option>
            <option value="latest_only">Latest only</option>
          </select>
        </label>
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="schedule-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
      </div>
      <div className="facts">
        <div>
          <dt>Last status</dt>
          <dd>{schedule?.lastStatus ?? "none"}</dd>
        </div>
        <div>
          <dt>Fire count</dt>
          <dd>{schedule?.fireCount ?? 0}</dd>
        </div>
        <div>
          <dt>Next run</dt>
          <dd>{schedule?.nextRunAt ?? "not scheduled"}</dd>
        </div>
        <div>
          <dt>Last attempt</dt>
          <dd>{schedule?.lastAttemptAt ?? "none"}</dd>
        </div>
        <div>
          <dt>Last error</dt>
          <dd>{schedule?.lastError ?? "none"}</dd>
        </div>
      </div>
      <div className="config-actions">
        {schedule ? (
          <button
            className="ghost-button danger-button"
            data-testid="schedule-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Schedule
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete schedule"
          message={`Delete schedule "${schedule?.scheduleId ?? ""}"? This automation stops firing immediately.`}
          confirmLabel="Delete Schedule"
          danger
          onConfirm={() => {
            void deleteSchedule();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="schedule-save"
          disabled={
            saving ||
            !scheduleId.trim() ||
            !taskId.trim() ||
            !intervalSecs.trim() ||
            !intervalValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Timer"}
        </button>
      </div>

      <section className="config-runner">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Manual Run</p>
            <h3>{scheduleId || "Timer Trigger"}</h3>
          </div>
          {runStatus ? (
            <span className="chip chip-green" data-testid="schedule-run-status">
              {runStatus.requestId}
            </span>
          ) : null}
        </div>
        <div className="config-actions">
          <button
            className="ghost-button"
            data-testid="schedule-run"
            disabled={runningTask || !schedule || !scheduleId.trim()}
            onClick={() => void runSelectedSchedule()}
            type="button"
          >
            {runningTask ? "Running..." : "Run Timer Now"}
          </button>
        </div>
      </section>
    </form>
  );
}

function scheduleFormValues(
  schedule: ScheduleView | null,
  fallbackTaskId: string | null,
) {
  return {
    scheduleId: schedule?.scheduleId ?? "",
    taskId: schedule?.taskId ?? fallbackTaskId ?? "",
    intervalSecs: schedule?.intervalSecs != null ? String(schedule.intervalSecs) : "",
    enabled: schedule?.enabled ?? true,
    concurrency: schedule?.concurrency ?? "serial",
  };
}
