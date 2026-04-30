import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  BehaviorView,
  DeploymentView,
  TaskRunResult,
  TaskSaveRequest,
  TaskView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import { optionalString } from "./formUtils";

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
        behaviors={deployment.behaviors}
        runningTask={runningTask}
        savedStatus={savedStatus}
        saving={saving}
        selectedBehavior={selectedBehavior}
        task={selectedTask}
        onRunTask={onRunTask}
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
  behaviors: BehaviorView[];
  selectedBehavior: BehaviorView | null;
  task: TaskView | null;
  savedStatus: string | null;
  saving: boolean;
  runningTask: boolean;
  onSaved: (taskId: string) => void;
  onSaveTaskConfig: (request: TaskSaveRequest) => Promise<unknown>;
  onRunTask: (request: { taskId: string; args?: unknown }) => Promise<TaskRunResult>;
};

export function TaskConfigEditor({
  behaviors,
  selectedBehavior,
  task,
  savedStatus,
  saving,
  runningTask,
  onSaved,
  onSaveTaskConfig,
  onRunTask,
}: TaskConfigEditorProps) {
  const [taskId, setTaskId] = useState("");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [behaviorId, setBehaviorId] = useState("");
  const [promptTemplate, setPromptTemplate] = useState("");
  const [outputSchemaRef, setOutputSchemaRef] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [runArgs, setRunArgs] = useState("{}");
  const [runStatus, setRunStatus] = useState<TaskRunResult | null>(null);

  useEffect(() => {
    setTaskId(task?.taskId ?? "");
    setName(task?.name ?? task?.taskId ?? "");
    setDescription(task?.description ?? "");
    setBehaviorId(task?.behaviorId ?? selectedBehavior?.behaviorId ?? "");
    setPromptTemplate(task?.promptTemplate ?? "");
    setOutputSchemaRef(task?.outputSchemaRef ?? "");
    setEnabled(task?.enabled ?? true);
    setRunStatus(null);
  }, [selectedBehavior?.behaviorId, task]);

  const runArgsValid = isJsonObject(runArgs);

  async function submitTask(event: FormEvent) {
    event.preventDefault();
    const nextId = taskId.trim();
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
  }

  async function runSelectedTask() {
    const result = await onRunTask({
      taskId: taskId.trim(),
      args: JSON.parse(runArgs || "{}") as unknown,
    });
    setRunStatus(result);
  }

  return (
    <form className="panel config-editor" onSubmit={submitTask}>
      <ConfigEditorHeader
        eyebrow="Task"
        saved={savedStatus === `task:${taskId.trim()}`}
        title={name || taskId || "New Task"}
      />
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
