/* Tasks: TaskSaveRequest fields, the run facts, and a manual run with
   JSON args. */
import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "./enabled";
import { dependentsWarning } from "./dependents";
import { useState } from "react";
import { toast } from "sonner";
import type { TaskView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Textarea } from "@gents/ui/components/textarea";
import {
  AreaRow,
  DraftActions,
  FactRow,
  NumberRow,
  RefRow,
  SwitchRow,
  TextRow,
  TagsRow,
} from "./editors";
import { optionalInteger, problemOf, str, useDraft } from "./draft";
import { HooksRows, hooksFromDraft, toHookDraft } from "./HooksRows";
import { DeleteButton, ListDetail } from "./ListDetail";
import { newId } from "./draft";
import { Group, Row } from "./rows";
import { RowMenu } from "./RowMenu";
import { BehaviorSheet } from "./BehaviorSheet";
import { NewAutomationDialog } from "./NewAutomationDialog";
import { EditorSheet } from "./EditorSheet";
import { TriggerEditor } from "./TriggersPanel";
import { sourceInWords, triggerReadiness } from "./automation";
import { Switch } from "@gents/ui/components/switch";
import { ExternalLink, Plus } from "lucide-react";
import { toastFailure } from "@/lib/failure";
import { agentOf } from "@/lib/agents";
import { useApp } from "@/app/AppContext";

const when = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString() : "—";

export function TaskEditor({
  deployment,
  task,
  embedded = false,
}: {
  deployment: NodeView;
  task: TaskView;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const {
    actions: { acceptsComposeIntent, captureComposeIntent, runTask, changeConfig },
  } = useApp();
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
    emitOutcome: task.emitOutcome ?? false,
    goalObjectiveTemplate: task.goalObjectiveTemplate ?? "",
    goalTokenBudget: str(task.goalTokenBudget),
    outputSchemaRef: task.outputSchemaRef ?? "",
    hooks: (task.hooks ?? []).map(toHookDraft),
    tags: task.tags ?? [],
  };
  const budget = (n: typeof saved) =>
    optionalInteger("Goal token budget", n.goalTokenBudget, { min: 1 });
  const d = useDraft(
    saved,
    (n) => {
      const hooks = hooksFromDraft(n.hooks);
      if (typeof hooks === "string") return Promise.reject(new Error(hooks));
      return changeConfig("saveTaskConfig", {
        document: {
          agent_did: deployment.agentDid,
          task_id: task.taskId,
          display_name: n.name.trim() || task.taskId,
          description: n.description || null,
          behavior_id: n.behaviorId,
          prompt_template: n.promptTemplate,
          emit_outcome: n.emitOutcome,
          goal_objective_template: n.goalObjectiveTemplate || null,
          goal_token_budget: budget(n),
          enabled: n.enabled,
          output_schema_ref: n.outputSchemaRef || null,
          hooks: hooks.length ? hooks : null,
          tags: n.tags.length ? n.tags : null,
        },
      });
    },
    {
      problems: (n) => {
        const hooks = hooksFromDraft(n.hooks);
        return {
          behaviorId: deployment.behaviors.some((b) => b.behaviorId === n.behaviorId)
            ? undefined
            : "Choose an existing behavior",
          promptTemplate: n.promptTemplate.trim()
            ? undefined
            : "Prompt template is required",
          goalObjectiveTemplate:
            n.goalTokenBudget.trim() && !n.goalObjectiveTemplate.trim()
              ? "A goal budget needs a goal objective"
              : undefined,
          goalTokenBudget: problemOf(() => budget(n)),
          hooks: typeof hooks === "string" ? hooks : undefined,
        };
      },
    },
  );
  /* the New behavior dialog's resolver while it is open */
  const [newBehavior, setNewBehavior] = useState<((id: string | null) => void) | null>(
    null,
  );
  /* this task's triggers, and the one open beside the page */
  const myTriggers = deployment.triggers.filter(
    (x) => x.config.task_id === task.taskId,
  );
  const [besideTrigger, setBesideTrigger] = useState<string | null>(null);
  const besideTriggerView =
    deployment.triggers.find((x) => x.config.trigger_id === besideTrigger) ?? null;
  /* a schedule or an event for this task: a dialog that writes nothing
     until Create, then the new trigger beside the page to adjust */
  const [adding, setAdding] = useState<"schedule" | "event" | null>(null);
  const [args, setArgs] = useState("{}");
  const [lastRun, setLastRun] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const id = (f: string) => `${task.taskId}-${f}`;
  const behaviors = deployment.behaviors.map((b) => ({
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
    const pending = runTask({
      taskId: task.taskId,
      agentDid: deployment.agentDid,
      args: parsed,
    });
    const intentGeneration = captureComposeIntent();
    try {
      const r = await pending;
      if (!acceptsComposeIntent(intentGeneration)) return;
      setLastRun(r.requestId);
      toast(`Task started · ${r.requestId}`);
    } catch (error) {
      if (!acceptsComposeIntent(intentGeneration)) return;
      toastFailure("start the task", error);
    } finally {
      setRunning(false);
    }
  };
  return (
    <>
      <Group title={embedded ? undefined : (task.name ?? task.taskId)}>
        <FactRow label="Task ID" mono>
          {task.taskId}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Name"
          value={d.draft.name}
          onChange={(v) => d.set("name", v)}
        />
        <RefRow
          id={id("behavior")}
          label="Behavior"
          description="Runs the prompt with its instructions, tools and model."
          value={d.draft.behaviorId}
          error={d.problems.behaviorId}
          onChange={(v) => d.set("behaviorId", v)}
          items={behaviors}
          none="Unset"
          createLabel="New behavior…"
          onCreate={() =>
            new Promise<string | null>((resolve) => {
              setNewBehavior(() => resolve);
            })
          }
          openRoute={(behaviorId) => ({
            ...base,
            section: "behaviors",
            item: behaviorId,
          })}
        />
        <BehaviorSheet
          deployment={deployment}
          open={newBehavior !== null}
          onClose={(behaviorId) => {
            newBehavior?.(behaviorId);
            setNewBehavior(null);
          }}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.set("enabled", v)}
        />
        <AreaRow
          id={id("desc")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          rows={2}
        />
        <AreaRow
          id={id("prompt")}
          label="Prompt template"
          description="Use {{ session.session_id }} and {{ request.request_id }} for this invocation’s identity."
          value={d.draft.promptTemplate}
          error={d.problems.promptTemplate}
          onChange={(v) => d.set("promptTemplate", v)}
          rows={5}
          stacked
        />
        <AreaRow
          id={id("goal")}
          label="Durable goal objective"
          description="Optional. Applies this goal when the queued request starts."
          value={d.draft.goalObjectiveTemplate}
          error={d.problems.goalObjectiveTemplate}
          onChange={(v) => d.set("goalObjectiveTemplate", v)}
          rows={2}
          stacked
        />
        <NumberRow
          id={id("budget")}
          label="Goal token budget"
          description="Optional positive whole number; blank leaves the goal unlimited. Needs an objective."
          value={d.draft.goalTokenBudget}
          error={d.problems.goalTokenBudget}
          onChange={(v) => d.set("goalTokenBudget", v)}
          placeholder="Optional"
        />
        <SwitchRow
          id={id("outcome")}
          label="Emit outcome"
          description="Publish one FireOutcome when the request finishes, or when its goal completes, blocks or exhausts its budget. Leave off for tasks that consume outcomes."
          checked={d.draft.emitOutcome}
          onChange={(v) => d.set("emitOutcome", v)}
        />
        <TextRow
          id={id("schema")}
          label="Output schema ref"
          value={d.draft.outputSchemaRef}
          onChange={(v) => d.set("outputSchemaRef", v)}
          mono
        />
        <HooksRows
          id={id("hooks")}
          value={d.draft.hooks}
          error={d.problems.hooks}
          onChange={(v) => d.set("hooks", v)}
        />
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        draft={d}
        fields={{
          behaviorId: id("behavior"),
          promptTemplate: id("prompt"),
          goalObjectiveTemplate: id("goal"),
          goalTokenBudget: id("budget"),
          hooks: `${id("hooks")}-0-id`,
        }}
      />
      <Group
        title="When it runs"
        action={
          <span className="flex gap-1">
            <Button size="sm" variant="outline" onClick={() => setAdding("schedule")}>
              <Plus /> Schedule
            </Button>
            <Button size="sm" variant="outline" onClick={() => setAdding("event")}>
              <Plus /> Event
            </Button>
          </span>
        }
      >
        {myTriggers.length === 0 && (
          <Row
            label="When you run it"
            description="No schedule or event yet; Run task below starts it."
          />
        )}
        {myTriggers.map((tr) => {
          const r = triggerReadiness(deployment, tr);
          return (
            <Row
              key={tr.config.trigger_id}
              label={sourceInWords(deployment, tr) ?? "A missing source"}
              description={r.ok ? (r.note ?? undefined) : `Won’t fire: ${r.reason}`}
            >
              <span className="flex items-center gap-1">
                <Switch
                  aria-label={`${tr.config.display_name ?? tr.config.trigger_id} enabled`}
                  checked={tr.config.enabled !== false}
                  onCheckedChange={(next) =>
                    void setEnabled(
                      changeConfig,
                      deployment.agentDid,
                      "Trigger",
                      tr.config.trigger_id,
                      next,
                    ).catch((e: unknown) => toastFailure("turn it on or off", e))
                  }
                />
                <Button
                  variant="quiet"
                  size="icon"
                  aria-label="Open trigger"
                  onClick={() => setBesideTrigger(tr.config.trigger_id)}
                >
                  <ExternalLink />
                </Button>
              </span>
            </Row>
          );
        })}
      </Group>
      <NewAutomationDialog
        key={adding ?? "closed"}
        deployment={deployment}
        open={adding !== null}
        onOpenChange={(open) => {
          if (!open) setAdding(null);
        }}
        task={task.taskId}
        initialKind={adding ?? "schedule"}
        onCreated={(triggerId) => setBesideTrigger(triggerId)}
      />
      <EditorSheet
        open={besideTriggerView !== null}
        onClose={() => setBesideTrigger(null)}
        title={besideTriggerView?.config.display_name ?? "Trigger"}
        description={
          besideTriggerView
            ? (sourceInWords(deployment, besideTriggerView) ?? undefined)
            : undefined
        }
        page={
          besideTriggerView
            ? {
                ...base,
                section: "triggers",
                item: besideTriggerView.config.trigger_id,
              }
            : undefined
        }
      >
        {besideTriggerView && (
          <TriggerEditor
            key={besideTriggerView.config.trigger_id}
            deployment={deployment}
            trigger={besideTriggerView}
            embedded
          />
        )}
      </EditorSheet>
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
      {!embedded && (
        <DeleteButton
          label={task.name ?? task.taskId}
          warning={dependentsWarning(deployment, "task", task.taskId)}
          base={base}
          onDelete={() =>
            changeConfig("deleteTaskConfig", {
              taskId: task.taskId,
              agentDid: deployment.agentDid,
            })
          }
        />
      )}
    </>
  );
}

/* when a task runs, from its triggers, and the first reason it would not */
function whenItRuns(deployment: NodeView, t: TaskView) {
  const triggers = deployment.triggers.filter((x) => x.config.task_id === t.taskId);
  if (t.enabled === false) return { when: "Disabled", problem: "Task is disabled" };
  if (!triggers.length) return { when: "Runs when you run it", problem: null };
  const whens = triggers.map((x) => sourceInWords(deployment, x) ?? "a missing source");
  const bad = triggers.map((x) => triggerReadiness(deployment, x)).find((r) => !r.ok);
  return {
    when:
      whens.length <= 2
        ? whens.join(" and ")
        : `${whens[0]} and ${whens.length - 1} more`,
    problem: bad && !bad.ok ? bad.reason : null,
  };
}

/* the canonical document for a task view, for row edits and copies */
function taskDocument(deployment: NodeView, t: TaskView) {
  return {
    agent_did: deployment.agentDid,
    task_id: t.taskId,
    display_name: t.name ?? t.taskId,
    description: t.description,
    behavior_id: t.behaviorId ?? "",
    prompt_template: t.promptTemplate ?? "",
    emit_outcome: t.emitOutcome,
    goal_objective_template: t.goalObjectiveTemplate,
    goal_token_budget: t.goalTokenBudget,
    hooks: t.hooks.length ? t.hooks : null,
    enabled: t.enabled ?? true,
    output_schema_ref: t.outputSchemaRef,
    tags: t.tags.length ? t.tags : null,
  };
}

export function TasksPanel({
  deployment,
  item,
}: {
  deployment: NodeView;
  item?: string;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "tasks",
  };
  const [creating, setCreating] = useState(false);
  return (
    <>
      <NewAutomationDialog
        deployment={deployment}
        open={creating}
        onOpenChange={setCreating}
        forTask
      />
      <ListDetail
        base={base}
        item={item}
        rows={deployment.tasks.map((t) => {
          const w = whenItRuns(deployment, t);
          return {
            id: t.taskId,
            title: t.name ?? t.taskId,
            tags: t.tags,
            meta: `${w.when} · with ${agentOf(deployment, t.behaviorId)?.displayName ?? "no behavior"}`,
            badge:
              w.problem ??
              (t.recentRuns.lastStatus === "failed" ? "last run failed" : undefined),
            badgeTone: "bad" as const,
            trailing: (
              <RowMenu
                name={t.name ?? t.taskId}
                base={base}
                id={t.taskId}
                enabled={{
                  checked: t.enabled !== false,
                  onChange: (enabled) =>
                    setEnabled(
                      changeConfig,
                      deployment.agentDid,
                      "Task",
                      t.taskId,
                      enabled,
                    ),
                }}
                onDuplicate={async () => {
                  const task_id = newId("task");
                  await changeConfig("saveTaskConfig", {
                    document: {
                      ...taskDocument(deployment, t),
                      task_id,
                      display_name: `${t.name ?? t.taskId} copy`,
                    },
                  });
                  return task_id;
                }}
                onDelete={() =>
                  changeConfig("deleteTaskConfig", {
                    taskId: t.taskId,
                    agentDid: deployment.agentDid,
                  })
                }
                warning={dependentsWarning(deployment, "task", t.taskId)}
              />
            ),
          };
        })}
        createLabel="New task"
        empty="No tasks. A task is a prompt a behavior runs: when you run it, on a schedule, or when something happens."
        onCreate={() => setCreating(true)}
        detail={(id) => {
          const task = deployment.tasks.find((t) => t.taskId === id)!;
          return <TaskEditor key={task.taskId} deployment={deployment} task={task} />;
        }}
      />
    </>
  );
}
