import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  BehaviorView,
  DeploymentView,
  TaskRunResult,
  TaskDeleteRequest,
  TaskSaveRequest,
  TaskView,
} from "../../lib/types";
import { ConfirmDialog } from "../ConfirmDialog";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { ignoreHandledActionError, optionalString } from "./formUtils";

export type TaskConfigPanelProps = {
  deployment: DeploymentView;
  selectedBehavior: BehaviorView | null;
  selectedTaskId: string | null;
  saving: boolean;
  runningTask: boolean;
  savedStatus: string | null;
  onSelectTask: (taskId: string) => void;
  onCreateTask: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveTaskConfig: (request: TaskSaveRequest) => Promise<unknown>;
  onDeleteTaskConfig: (request: TaskDeleteRequest) => Promise<unknown>;
  onDeletedTask: () => void;
  onRunTask: (request: { taskId: string; args?: unknown }) => Promise<TaskRunResult>;
};

export function TaskConfigPanel({
  deployment,
  selectedBehavior,
  selectedTaskId,
  saving,
  runningTask,
  savedStatus,
  onSelectTask,
  onCreateTask,
  onSavedStatusChange,
  onSaveTaskConfig,
  onDeleteTaskConfig,
  onDeletedTask,
  onRunTask,
}: TaskConfigPanelProps) {
  const selectedTask = useMemo(
    () => deployment.tasks.find((task) => task.taskId === selectedTaskId) ?? null,
    [deployment.tasks, selectedTaskId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Tasks"
        items={deployment.tasks.map((task) => {
          const title = displayTaskListTitle(task);
          return {
            id: task.taskId,
            title,
            meta: title === task.taskId ? "task" : task.taskId,
          };
        })}
        selectedId={selectedTaskId}
        testPrefix="task"
        title="Task Prompts"
        onCreate={onCreateTask}
        onSelect={onSelectTask}
      />

      <TaskConfigEditor
        agentDid={deployment.agentDid}
        behaviors={deployment.behaviors}
        runningTask={runningTask}
        savedStatus={savedStatus}
        saving={saving}
        selectedBehavior={selectedBehavior}
        task={selectedTask}
        onRunTask={onRunTask}
        onDeleteTaskConfig={onDeleteTaskConfig}
        onDeleted={() => {
          onDeletedTask();
        }}
        onSaved={(taskId) => {
          onSelectTask(taskId);
          onSavedStatusChange(`task:${taskId}`);
        }}
        onSaveTaskConfig={onSaveTaskConfig}
      />
    </section>
  );
}

export type TaskConfigEditorProps = {
  agentDid: string;
  behaviors: BehaviorView[];
  selectedBehavior: BehaviorView | null;
  task: TaskView | null;
  savedStatus: string | null;
  saving: boolean;
  runningTask: boolean;
  onSaved: (taskId: string) => void;
  onSaveTaskConfig: (request: TaskSaveRequest) => Promise<unknown>;
  onDeleteTaskConfig: (request: TaskDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
  onRunTask: (request: { taskId: string; args?: unknown }) => Promise<TaskRunResult>;
};

export function TaskConfigEditor({
  agentDid,
  behaviors,
  selectedBehavior,
  task,
  savedStatus,
  saving,
  runningTask,
  onSaved,
  onSaveTaskConfig,
  onDeleteTaskConfig,
  onDeleted,
  onRunTask,
}: TaskConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteTask() {
    setConfirmingDelete(false);
    if (!task) {
      return;
    }
    try {
      await onDeleteTaskConfig({ taskId: task.taskId, agentDid });
      onDeleted();
    } catch {
      // Surfaced by the shell error banner; the editor stays put.
    }
  }
  const [taskId, setTaskId] = useState("");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [behaviorId, setBehaviorId] = useState("");
  const [promptTemplate, setPromptTemplate] = useState("");
  const [outputSchemaRef, setOutputSchemaRef] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [runArgs, setRunArgs] = useState("{}");
  const [runStatus, setRunStatus] = useState<TaskRunResult | null>(null);

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const b = taskFormValues(task, selectedBehavior?.behaviorId ?? null);
    setTaskId(b.taskId);
    setName(b.name);
    setDescription(b.description);
    setBehaviorId(b.behaviorId);
    setPromptTemplate(b.promptTemplate);
    setOutputSchemaRef(b.outputSchemaRef);
    setEnabled(b.enabled);
    setSaveError(null);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
  }, [selectedBehavior?.behaviorId, task?.taskId]);

  useEffect(() => {
    setRunStatus(null);
  }, [selectedBehavior?.behaviorId, task?.taskId]);

  const runArgsValid = isJsonObject(runArgs);

  async function submitTask(event: FormEvent) {
    event.preventDefault();
    const nextId = taskId.trim();
    try {
      await onSaveTaskConfig({
        taskId: nextId,
        name,
        description: optionalString(description),
        behaviorId,
        promptTemplate,
        enabled,
        outputSchemaRef: optionalString(outputSchemaRef),
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  async function runSelectedTask() {
    try {
      const result = await onRunTask({
        taskId: taskId.trim(),
        args: JSON.parse(runArgs || "{}") as unknown,
      });
      setRunStatus(result);
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitTask}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            taskId,
            name,
            description,
            behaviorId,
            promptTemplate,
            outputSchemaRef,
            enabled,
          },
          taskFormValues(task, selectedBehavior?.behaviorId ?? null),
        )}
        eyebrow="Task"
        saved={savedStatus === `task:${taskId.trim()}`}
        title={name || taskId || "New Task"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Task ID</span>
          <input
            data-testid="task-id"
            onChange={(event) => {
              if (!task) {
                setTaskId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(task)}
            title={task ? "Task IDs cannot be renamed after creation." : undefined}
            value={taskId}
          />
        </label>
        <label className="field">
          <span>Name</span>
          <input
            data-testid="task-name"
            onChange={(event) => setName(event.currentTarget.value)}
            value={name}
          />
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Behavior</span>
          <select
            data-testid="task-behavior-id"
            onChange={(event) => setBehaviorId(event.currentTarget.value)}
            value={behaviorId}
          >
            <option value="">Unset</option>
            {behaviors.map((behavior) => (
              <option key={behavior.behaviorId} value={behavior.behaviorId}>
                {behavior.displayName}
              </option>
            ))}
          </select>
        </label>
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="task-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
      </div>
      <label className="field">
        <span>Description</span>
        <textarea
          className="config-small-textarea"
          data-testid="task-description"
          onChange={(event) => setDescription(event.currentTarget.value)}
          value={description}
        />
      </label>
      <label className="field">
        <span>Prompt template</span>
        <textarea
          className="config-textarea"
          data-testid="task-prompt-template"
          onChange={(event) => setPromptTemplate(event.currentTarget.value)}
          value={promptTemplate}
        />
      </label>
      <label className="field">
        <span>Output schema ref</span>
        <input
          data-testid="task-output-schema-ref"
          onChange={(event) => setOutputSchemaRef(event.currentTarget.value)}
          value={outputSchemaRef}
        />
      </label>
      <div className="config-actions">
        {task ? (
          <button
            className="ghost-button danger-button"
            data-testid="task-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Task
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete task"
          message={`Delete task "${task?.taskId ?? ""}"? Schedules or triggers still referencing it will block the delete.`}
          confirmLabel="Delete Task"
          danger
          onConfirm={() => {
            void deleteTask();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="task-save"
          disabled={
            saving ||
            !taskId.trim() ||
            !name.trim() ||
            !behaviorId.trim() ||
            !promptTemplate.trim()
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Task"}
        </button>
      </div>

      <section className="config-runner">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Runs</p>
            <h3>{taskId || "Task"}</h3>
          </div>
        </div>
        <div className="facts">
          <div>
            <dt>Total fires</dt>
            <dd>{task?.recentRuns.totalFires ?? 0}</dd>
          </div>
          <div>
            <dt>Last attempt</dt>
            <dd>{task?.recentRuns.lastAttemptAt ?? "none"}</dd>
          </div>
          <div>
            <dt>Last status</dt>
            <dd>{task?.recentRuns.lastStatus ?? "none"}</dd>
          </div>
          <div>
            <dt>Last error</dt>
            <dd>{task?.recentRuns.lastError ?? "none"}</dd>
          </div>
        </div>
        <div className="run-history" data-testid="task-run-history">
          {runStatus ? (
            <div className="run-history-row">
              <span className="mono">{runStatus.requestId}</span>
              <span>manual</span>
              <span>{runStatus.lifecycleState ?? runStatus.status ?? "submitted"}</span>
            </div>
          ) : null}
          {(task?.runHistory ?? []).map((run) => (
            <div className="run-history-row" key={run.requestId}>
              <span className="mono">{run.requestId}</span>
              <span>
                {run.causedByTriggerKind ?? "trigger"}:
                {run.causedByTriggerId ?? "unknown"}
              </span>
              <span>{run.lifecycleState ?? run.status ?? "pending"}</span>
            </div>
          ))}
          {!runStatus && !(task?.runHistory ?? []).length ? (
            <p className="muted">No recorded runs.</p>
          ) : null}
        </div>
      </section>

      <section className="config-runner">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Manual Run</p>
            <h3>{taskId || "Task"}</h3>
          </div>
          {runStatus ? (
            <span className="chip chip-green" data-testid="task-run-status">
              {runStatus.requestId}
            </span>
          ) : null}
        </div>
        <label className="field">
          <span>Args JSON</span>
          <textarea
            className="config-small-textarea"
            data-testid="task-run-args"
            onChange={(event) => setRunArgs(event.currentTarget.value)}
            value={runArgs}
          />
          <FieldHint show={!runArgsValid}>Must be a JSON object</FieldHint>
        </label>
        <div className="config-actions">
          <button
            className="ghost-button"
            data-testid="task-run"
            disabled={runningTask || !task || !taskId.trim() || !runArgsValid}
            onClick={() => void runSelectedTask()}
            type="button"
          >
            {runningTask ? "Running..." : "Run Task"}
          </button>
        </div>
      </section>
    </form>
  );
}

function isJsonObject(value: string) {
  try {
    const parsed = JSON.parse(value || "{}") as unknown;
    return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed);
  } catch {
    return false;
  }
}

function displayTaskListTitle(task: TaskView) {
  const name = task.name?.trim();
  if (name && name.toLowerCase() !== "default") {
    return name;
  }

  return task.taskId;
}

/** View→form hydration, shared by the reset effect and dirty comparison. */
function taskFormValues(task: TaskView | null, fallbackBehaviorId: string | null) {
  return {
    taskId: task?.taskId ?? "",
    name: task?.name ?? task?.taskId ?? "",
    description: task?.description ?? "",
    behaviorId: task?.behaviorId ?? fallbackBehaviorId ?? "",
    promptTemplate: task?.promptTemplate ?? "",
    outputSchemaRef: task?.outputSchemaRef ?? "",
    enabled: task?.enabled ?? true,
  };
}
