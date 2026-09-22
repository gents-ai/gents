/* Tasks, as the desktop app's Tasks tab: TaskSaveRequest fields, the
   run facts, and a manual run with JSON args. */
import { useState } from "react";
import { toast } from "sonner";
import type { DeploymentView, TaskView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Textarea } from "@gents/ui/components/textarea";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  SwitchRow,
  TextRow,
} from "./editors";
import { fromLinesOrNull, optionalInteger, str, toLines, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { newId } from "./draft";
import { Group, Row } from "./rows";

const when = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString() : "—";

function Editor({
  shell,
  deployment,
  task,
}: {
  shell: Shell;
  deployment: DeploymentView;
  task: TaskView;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "tasks",
  };
  const saved = {
    name: task.name ?? "",
    behaviorId: task.behaviorId ?? "",
    enabled: task.enabled ?? true,
    description: task.description ?? "",
    promptTemplate: task.promptTemplate ?? "",
    goalObjectiveTemplate: task.goalObjectiveTemplate ?? "",
    goalTokenBudget: str(task.goalTokenBudget),
    outputSchemaRef: task.outputSchemaRef ?? "",
    hooks: JSON.stringify(task.hooks ?? [], null, 2),
    tags: toLines(task.tags ?? []),
  };
  const d = useDraft(saved, (n) => {
    if (!deployment.behaviors.some((behavior) => behavior.behaviorId === n.behaviorId))
      return Promise.reject(new Error("Choose an existing behaviour"));
    if (!n.promptTemplate.trim())
      return Promise.reject(new Error("Prompt template is required"));
    if (n.goalTokenBudget.trim() && !n.goalObjectiveTemplate.trim()) {
      return Promise.reject(new Error("A goal budget needs a goal objective"));
    }
    let hooks: typeof task.hooks;
    try {
      hooks = JSON.parse(n.hooks) as typeof task.hooks;
      if (!Array.isArray(hooks)) throw new Error();
    } catch {
      return Promise.reject(new Error("Hooks must be a JSON array"));
    }
    const hookIds = new Set<string>();
    for (const hook of hooks) {
      if (!hook || typeof hook !== "object")
        return Promise.reject(new Error("Every hook must be a JSON object"));
      if (!hook.hook_id?.trim())
        return Promise.reject(new Error("Every hook needs a hook_id"));
      if (hookIds.has(hook.hook_id))
        return Promise.reject(new Error(`Duplicate hook ID: ${hook.hook_id}`));
      hookIds.add(hook.hook_id);
      if (!["before", "after_success", "after_failure", "finally"].includes(hook.phase))
        return Promise.reject(new Error(`Invalid hook phase: ${String(hook.phase)}`));
      if (
        !Array.isArray(hook.command) ||
        !hook.command.length ||
        !hook.command[0]?.trim()
      )
        return Promise.reject(new Error(`Hook ${hook.hook_id} needs a command`));
      if (
        hook.timeout_secs != null &&
        (!Number.isInteger(hook.timeout_secs) || hook.timeout_secs < 1)
      )
        return Promise.reject(
          new Error(`Hook ${hook.hook_id} timeout must be a positive whole number`),
        );
    }
    return shell.applyConfig((api) =>
      api.saveTaskConfig({
        document: {
          agent_did: deployment.agentDid,
          task_id: task.taskId,
          display_name: n.name.trim() || task.taskId,
          description: n.description || null,
          behavior_id: n.behaviorId,
          prompt_template: n.promptTemplate,
          goal_objective_template: n.goalObjectiveTemplate || null,
          goal_token_budget: optionalInteger("Goal token budget", n.goalTokenBudget, {
            min: 1,
          }),
          enabled: n.enabled,
          output_schema_ref: n.outputSchemaRef || null,
          hooks: hooks.length ? hooks : null,
          tags: fromLinesOrNull(n.tags),
        },
      }),
    );
  });
  const [args, setArgs] = useState("{}");
  const [lastRun, setLastRun] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const id = (f: string) => `${task.taskId}-${f}`;
  const behaviours = deployment.behaviors.map((b) => ({
    value: b.behaviorId,
    label: b.displayName,
  }));
  const run = async () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(args);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
        throw new Error();
    } catch {
      toast("Args must be a JSON object");
      return;
    }
    setRunning(true);
    const pending = shell.runTask({
      taskId: task.taskId,
      agentDid: deployment.agentDid,
      args: parsed,
    });
    const intentGeneration = shell.captureComposeIntent();
    try {
      const r = await pending;
      if (!shell.acceptsComposeIntent(intentGeneration)) return;
      setLastRun(r.requestId);
      toast(`Task started · ${r.requestId}`);
    } catch (error) {
      if (!shell.acceptsComposeIntent(intentGeneration)) return;
      toast(
        `Task failed to start: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setRunning(false);
    }
  };
  return (
    <>
      <Group title={task.name ?? task.taskId}>
        <FactRow label="Task ID" mono>
          {task.taskId}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Name"
          value={d.draft.name}
          onChange={(v) => d.set("name", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("behavior")}
          label="Behaviour"
          value={d.draft.behaviorId}
          onChange={(v) => d.choose("behaviorId", v)}
          items={behaviours}
          none="Unset"
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <AreaRow
          id={id("desc")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          rows={2}
        />
        <AreaRow
          id={id("prompt")}
          label="Prompt template"
          value={d.draft.promptTemplate}
          onChange={(v) => d.set("promptTemplate", v)}
          onCommit={d.commit}
          rows={5}
        />
        <AreaRow
          id={id("goal")}
          label="Durable goal objective"
          description="Optional. Provisions this goal before the first request becomes runnable."
          value={d.draft.goalObjectiveTemplate}
          onChange={(v) => d.set("goalObjectiveTemplate", v)}
          onCommit={d.commit}
          rows={2}
        />
        <NumberRow
          id={id("budget")}
          label="Goal token budget"
          description="A positive whole number, or blank; needs an objective."
          value={d.draft.goalTokenBudget}
          onChange={(v) => d.set("goalTokenBudget", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          placeholder="Optional"
        />
        <TextRow
          id={id("schema")}
          label="Output schema ref"
          value={d.draft.outputSchemaRef}
          onChange={(v) => d.set("outputSchemaRef", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <AreaRow
          id={id("hooks")}
          label="Task hooks"
          description={
            <>
              Canonical JSON array. Commands are argv arrays, for example:
              <code className="mt-1 block w-0 min-w-full overflow-x-auto font-mono text-xs whitespace-pre">
                {
                  '[{"hook_id":"verify","phase":"after_success","command":["cargo","test"],"timeout_secs":120}]'
                }
              </code>
            </>
          }
          value={d.draft.hooks}
          onChange={(v) => d.set("hooks", v)}
          onCommit={d.commit}
          rows={8}
          mono
        />
        <AreaRow
          id={id("tags")}
          label="Tags"
          description="One per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={3}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      <Group title="Runs">
        <FactRow label="Total fires">{task.recentRuns.totalFires}</FactRow>
        <FactRow label="Last attempt">{when(task.recentRuns.lastAttemptAt)}</FactRow>
        <FactRow label="Last status">{task.recentRuns.lastStatus ?? "—"}</FactRow>
        <FactRow label="Last error">{task.recentRuns.lastError ?? "—"}</FactRow>
        <FactRow label="Wired to">
          {task.recentRuns.scheduleCount} schedules · {task.recentRuns.eventCount}{" "}
          events
        </FactRow>
        {task.runHistory.slice(0, 5).map((r) => (
          <FactRow
            key={r.requestId}
            label={<span className="font-mono text-xs">{r.requestId}</span>}
            description={`${r.causedByTriggerKind ?? "manual"}${r.causedByTriggerId ? `:${r.causedByTriggerId}` : ""}`}
          >
            {r.lifecycleState ?? "—"}
          </FactRow>
        ))}
      </Group>
      <Group
        title="Manual run"
        action={
          <Button size="sm" variant="brand" disabled={running} onClick={run}>
            {running ? "Starting…" : "Run task"}
          </Button>
        }
      >
        <Row label="Args" description="Must be a JSON object." htmlFor={id("args")}>
          <Textarea
            id={id("args")}
            value={args}
            onChange={(e) => setArgs(e.target.value)}
            rows={3}
            className="w-96 max-md:w-full font-mono text-xs"
          />
        </Row>
        {lastRun && (
          <FactRow label="Started" mono>
            {lastRun}
          </FactRow>
        )}
      </Group>
      <DeleteButton
        label={task.name ?? task.taskId}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteTaskConfig({
              taskId: task.taskId,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function TasksPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell;
  deployment: DeploymentView;
  item?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "tasks",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.tasks.map((t) => ({
        id: t.taskId,
        title: t.name ?? t.taskId,
        meta: `${deployment.behaviors.find((b) => b.behaviorId === t.behaviorId)?.displayName ?? "no behaviour"} · ${t.recentRuns.totalFires} fires${t.enabled === false ? " · disabled" : ""}`,
        badge: t.recentRuns.lastStatus ?? undefined,
        badgeTone: t.recentRuns.lastStatus === "failed" ? "bad" : "default",
        tags: t.tags,
      }))}
      createLabel="New task"
      empty="No tasks. A task is a prompt the agent runs on a schedule or a trigger."
      onCreate={async () => {
        const taskId = newId("task");
        await shell.applyConfig((api) =>
          api.saveTaskConfig({
            document: {
              agent_did: deployment.agentDid,
              task_id: taskId,
              display_name: "New task",
              description: null,
              behavior_id:
                deployment.behaviors.find((b) => b.isDefault)?.behaviorId ??
                deployment.behaviors[0]?.behaviorId ??
                "",
              prompt_template: "Describe what this task should do.",
              goal_objective_template: null,
              goal_token_budget: null,
              enabled: false,
              output_schema_ref: null,
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "tasks",
          item: taskId,
        });
      }}
      detail={(id) => {
        const task = deployment.tasks.find((t) => t.taskId === id)!;
        return (
          <Editor key={task.taskId} shell={shell} deployment={deployment} task={task} />
        );
      }}
    />
  );
}
