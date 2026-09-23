/* Triggers: each read as a sentence, with the task, schedule and event
   source it needs creatable in place. Tasks is the page people start on;
   this is the desktop's own tab. See CONFIG-FLOWS.md. */
import { useState } from "react";
import { Timer, Zap } from "lucide-react";
import type { DeploymentView, TriggerView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  RefRow,
  SwitchRow,
  TextRow,
  TagsRow,
} from "./editors";
import { newId, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import {
  actionInWords,
  cadenceInWords,
  eventInWords,
  sourceInWords,
  triggerReadiness,
} from "./automation";
import { NewAutomationDialog } from "./NewAutomationDialog";
import { EditorSheet } from "./EditorSheet";
import { TaskEditor } from "./TasksPanel";
import { ScheduleEditor } from "./SchedulesPanel";
import { EventSourceEditor } from "./EventSourcesPanel";
import { RowMenu } from "./RowMenu";

const SECTION = "triggers";
const when = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString() : "—";

export function TriggerEditor({
  shell,
  deployment,
  trigger,
  embedded = false,
}: {
  shell: Shell;
  deployment: DeploymentView;
  trigger: TriggerView;
  /* in a sheet beside a task: no Danger zone */
  embedded?: boolean;
}) {
  const cfg = trigger.config;
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: SECTION,
  };
  const saved = {
    displayName: cfg.display_name ?? "",
    description: cfg.description ?? "",
    taskId: cfg.task_id,
    enabled: cfg.enabled ?? true,
    concurrency: cfg.concurrency ?? "",
    sourceKind: cfg.source.kind,
    sourceId:
      cfg.source.kind === "schedule"
        ? cfg.source.schedule_id
        : cfg.source.event_source_id,
    tags: cfg.tags ?? [],
  };
  const d = useDraft(saved, async (next) => {
    if (!deployment.tasks.some((task) => task.taskId === next.taskId))
      throw new Error("Choose an existing task");
    const sourceExists =
      next.sourceKind === "schedule"
        ? deployment.schedules.some((row) => row.schedule_id === next.sourceId)
        : deployment.eventSources.some((row) => row.event_source_id === next.sourceId);
    if (!sourceExists) throw new Error("Choose an existing source");
    await shell.applyConfig((api) =>
      api.saveTriggerConfig({
        document: {
          ...cfg,
          display_name: next.displayName.trim() || null,
          description: next.description.trim() || null,
          task_id: next.taskId,
          enabled: next.enabled,
          concurrency: (next.concurrency || null) as
            "parallel" | "serial" | "latest_only" | null,
          source:
            next.sourceKind === "schedule"
              ? { kind: "schedule", schedule_id: next.sourceId }
              : { kind: "event", event_source_id: next.sourceId },
          tags: next.tags.length ? next.tags : null,
        },
      }),
    );
  });
  const id = (f: string) => `${cfg.trigger_id}-${f}`;
  const readiness = triggerReadiness(deployment, trigger);
  /* a referenced document's full editor, open beside the automation */
  const [beside, setBeside] = useState<{
    kind: "task" | "schedule" | "source";
    id: string;
  } | null>(null);
  const besideTask =
    beside?.kind === "task"
      ? deployment.tasks.find((t) => t.taskId === beside.id)
      : null;
  const besideSchedule =
    beside?.kind === "schedule"
      ? deployment.schedules.find((s) => s.schedule_id === beside.id)
      : null;
  const besideSource =
    beside?.kind === "source"
      ? deployment.eventSources.find((s) => s.event_source_id === beside.id)
      : null;
  return (
    <>
      <EditorSheet
        open={Boolean(besideTask || besideSchedule || besideSource)}
        onClose={() => setBeside(null)}
        title={
          besideTask?.name ??
          besideSchedule?.display_name ??
          besideSource?.display_name ??
          beside?.id ??
          ""
        }
        description={
          besideTask
            ? `Runs with ${deployment.behaviors.find((b) => b.behaviorId === besideTask.behaviorId)?.displayName ?? "no behavior"}`
            : besideSchedule
              ? cadenceInWords(besideSchedule)
              : besideSource
                ? eventInWords(besideSource)
                : undefined
        }
        page={
          beside
            ? {
                ...base,
                section:
                  beside.kind === "task"
                    ? "tasks"
                    : beside.kind === "schedule"
                      ? "schedules"
                      : "event-sources",
                item: beside.id,
              }
            : undefined
        }
      >
        {besideTask && (
          <TaskEditor
            key={besideTask.taskId}
            shell={shell}
            deployment={deployment}
            task={besideTask}
            embedded
          />
        )}
        {besideSchedule && (
          <ScheduleEditor
            key={besideSchedule.schedule_id}
            shell={shell}
            deployment={deployment}
            schedule={besideSchedule}
            embedded
          />
        )}
        {besideSource && (
          <EventSourceEditor
            key={besideSource.event_source_id}
            shell={shell}
            deployment={deployment}
            source={besideSource}
            embedded
          />
        )}
      </EditorSheet>
      <header className="mb-6">
        <h2 className="font-heading text-lg text-heading">
          {cfg.display_name ?? cfg.trigger_id}
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">
          {sourceInWords(deployment, trigger) ?? "No source"} ·{" "}
          {actionInWords(deployment, trigger)}
        </p>
        <p
          className={`mt-1 text-sm ${readiness.ok ? "text-muted-foreground" : "text-destructive"}`}
        >
          {readiness.ok
            ? (readiness.note ?? "Ready")
            : `Won’t fire: ${readiness.reason}`}
        </p>
      </header>
      <Group title="Trigger">
        <FactRow label="Trigger ID" mono>
          {cfg.trigger_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          rows={2}
        />
        <RefRow
          id={id("task")}
          label="Task"
          description="The prompt that runs, and the behavior that runs it."
          value={d.draft.taskId}
          onChange={(v) => d.choose("taskId", v)}
          items={deployment.tasks.map((t) => ({
            value: t.taskId,
            label: t.name ?? t.taskId,
          }))}
          createLabel="New task…"
          onCreate={async () => {
            const task_id = newId("task");
            await shell.applyConfig((api) =>
              api.saveTaskConfig({
                document: {
                  agent_did: deployment.agentDid,
                  task_id,
                  display_name: `${cfg.display_name ?? "Automation"} task`,
                  description: null,
                  behavior_id:
                    deployment.behaviors.find((b) => b.isDefault)?.behaviorId ??
                    deployment.behaviors[0]?.behaviorId ??
                    "",
                  prompt_template: "Describe what this task should do.",
                  goal_objective_template: null,
                  goal_token_budget: null,
                  enabled: true,
                  output_schema_ref: null,
                },
              }),
            );
            return task_id;
          }}
          onOpen={(taskId) => setBeside({ kind: "task", id: taskId })}
        />
        <ChoiceRow
          id={id("kind")}
          label="Source"
          value={d.draft.sourceKind}
          onChange={(v) => {
            const kind = v as "schedule" | "event";
            d.set("sourceKind", kind);
            d.set(
              "sourceId",
              kind === "schedule"
                ? (deployment.schedules[0]?.schedule_id ?? "")
                : (deployment.eventSources[0]?.event_source_id ?? ""),
            );
          }}
          items={[
            { value: "schedule", label: "Schedule" },
            { value: "event", label: "Event source" },
          ]}
        />
        {d.draft.sourceKind === "schedule" ? (
          <RefRow
            id={id("sid")}
            label="Schedule"
            value={d.draft.sourceId}
            onChange={(v) => d.choose("sourceId", v)}
            items={deployment.schedules.map((s) => ({
              value: s.schedule_id,
              label: s.display_name ?? cadenceInWords(s),
            }))}
            createLabel="New schedule…"
            onCreate={async () => {
              const schedule_id = newId("sched");
              const cadence = {
                kind: "cron" as const,
                expression: "0 9 * * *",
                timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
                missed_run_policy: "latest_only" as const,
              };
              await shell.applyConfig((api) =>
                api.saveScheduleConfig({
                  document: {
                    agent_did: deployment.agentDid,
                    schedule_id,
                    display_name: "Every day at 09:00",
                    cadence,
                  },
                }),
              );
              return schedule_id;
            }}
            onOpen={(scheduleId) => setBeside({ kind: "schedule", id: scheduleId })}
          />
        ) : (
          <RefRow
            id={id("sid")}
            label="Event source"
            value={d.draft.sourceId}
            onChange={(v) => d.choose("sourceId", v)}
            items={deployment.eventSources.map((s) => ({
              value: s.event_source_id,
              label: s.display_name ?? eventInWords(s),
            }))}
            createLabel="New event source…"
            onCreate={async () => {
              const event_source_id = newId("evsrc");
              await shell.applyConfig((api) =>
                api.saveEventSourceConfig({
                  document: {
                    agent_did: deployment.agentDid,
                    event_source_id,
                    display_name: "When an AgentRequest is created",
                    source_collection: "AgentRequest",
                    event_kind: "created",
                  },
                }),
              );
              return event_source_id;
            }}
            onOpen={(sourceId) => setBeside({ kind: "source", id: sourceId })}
          />
        )}
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <ChoiceRow
          id={id("concurrency")}
          label="Concurrency"
          value={d.draft.concurrency}
          onChange={(v) => d.choose("concurrency", v)}
          items={[
            { value: "parallel", label: "Parallel" },
            { value: "serial", label: "Serial" },
            { value: "latest_only", label: "Latest only" },
          ]}
          none="Default (parallel)"
        />
      </Group>
      <Group title="Runs">
        <FactRow label="Fired">{trigger.fireCount ?? 0} times</FactRow>
        <FactRow label="Last attempt">{when(trigger.lastAttemptAt)}</FactRow>
        <FactRow label="Last status">{trigger.lastStatus ?? "—"}</FactRow>
        {trigger.lastError && <FactRow label="Last error">{trigger.lastError}</FactRow>}
        <FactRow label="Next run">{when(trigger.nextRunAt)}</FactRow>
      </Group>
      <Group title="Metadata">
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      {!embedded && (
        <DeleteButton
          label={cfg.display_name ?? cfg.trigger_id}
          base={base}
          onDelete={() =>
            shell.applyConfig((api) =>
              api.deleteTriggerConfig({
                triggerId: cfg.trigger_id,
                agentDid: deployment.agentDid,
              }),
            )
          }
        />
      )}
    </>
  );
}

export function TriggersPanel({
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
    section: SECTION,
  };
  const [creating, setCreating] = useState(false);
  return (
    <>
      <NewAutomationDialog
        shell={shell}
        deployment={deployment}
        open={creating}
        onOpenChange={setCreating}
      />
      <ListDetail
        base={base}
        item={item}
        rows={deployment.triggers.map((t) => {
          const r = triggerReadiness(deployment, t);
          return {
            id: t.config.trigger_id,
            title: t.config.display_name ?? t.config.trigger_id,
            tags: t.config.tags,
            meta: `${sourceInWords(deployment, t) ?? "No source"} · ${actionInWords(deployment, t)}`,
            icon:
              t.config.source.kind === "schedule" ? (
                <Timer className="size-4" />
              ) : (
                <Zap className="size-4" />
              ),
            badge: r.ok
              ? (r.note ?? undefined)
              : r.reason === "Off"
                ? undefined
                : r.reason,
            badgeTone: r.ok ? "default" : "bad",
            trailing: (
              <RowMenu
                name={t.config.display_name ?? t.config.trigger_id}
                base={base}
                id={t.config.trigger_id}
                enabled={{
                  checked: t.config.enabled !== false,
                  onChange: (enabled) =>
                    shell.applyConfig((api) =>
                      api.saveTriggerConfig({ document: { ...t.config, enabled } }),
                    ),
                }}
                onDuplicate={async () => {
                  const trigger_id = newId("trig");
                  await shell.applyConfig((api) =>
                    api.saveTriggerConfig({
                      document: {
                        ...t.config,
                        trigger_id,
                        display_name: `${t.config.display_name ?? t.config.trigger_id} copy`,
                        enabled: false,
                        created_at: null,
                        updated_at: null,
                      },
                    }),
                  );
                  return trigger_id;
                }}
                onDelete={() =>
                  shell.applyConfig((api) =>
                    api.deleteTriggerConfig({
                      triggerId: t.config.trigger_id,
                      agentDid: deployment.agentDid,
                    }),
                  )
                }
              />
            ),
          };
        })}
        createLabel="New trigger"
        empty="No triggers. A trigger runs a task on a schedule or when something happens."
        onCreate={() => setCreating(true)}
        detail={(id) => {
          const trigger = deployment.triggers.find((t) => t.config.trigger_id === id)!;
          return (
            <TriggerEditor
              key={trigger.config.trigger_id}
              shell={shell}
              deployment={deployment}
              trigger={trigger}
            />
          );
        }}
      />
    </>
  );
}
