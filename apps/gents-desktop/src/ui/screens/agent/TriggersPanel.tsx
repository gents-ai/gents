import type { DeploymentView, TriggerView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  SwitchRow,
  TextRow,
} from "./editors";
import { fromLinesOrNull, newId, toLines, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";

function Editor({
  shell,
  deployment,
  trigger,
}: {
  shell: Shell;
  deployment: DeploymentView;
  trigger: TriggerView;
}) {
  const cfg = trigger.config;
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "triggers",
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
    tags: toLines(cfg.tags ?? []),
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
          tags: fromLinesOrNull(next.tags),
        },
      }),
    );
  });
  const id = (f: string) => `${cfg.trigger_id}-${f}`;
  return (
    <>
      <Group title={cfg.display_name ?? cfg.trigger_id}>
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
        <ChoiceRow
          id={id("task")}
          label="Task"
          value={d.draft.taskId}
          onChange={(v) => d.choose("taskId", v)}
          items={deployment.tasks.map((t) => ({
            value: t.taskId,
            label: t.name ?? t.taskId,
          }))}
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
        <ChoiceRow
          id={id("sid")}
          label={d.draft.sourceKind === "schedule" ? "Schedule" : "Event source"}
          value={d.draft.sourceId}
          onChange={(v) => d.choose("sourceId", v)}
          items={
            d.draft.sourceKind === "schedule"
              ? deployment.schedules.map((s) => ({
                  value: s.schedule_id,
                  label: s.display_name ?? s.schedule_id,
                }))
              : deployment.eventSources.map((s) => ({
                  value: s.event_source_id,
                  label: s.display_name ?? s.event_source_id,
                }))
          }
        />
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
      <Group title="Metadata">
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
    section: "triggers",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.triggers.map((t) => ({
        id: t.config.trigger_id,
        title: t.config.display_name ?? t.config.trigger_id,
        meta: t.config.source.kind,
      }))}
      createLabel="New trigger"
      empty="No triggers. A trigger fires a task from a schedule or event source."
      onCreate={async () => {
        const trigger_id = newId("trig");
        const task = deployment.tasks[0];
        const schedule = deployment.schedules[0];
        if (!task || !schedule) throw new Error("Add a task and schedule first");
        await shell.applyConfig((api) =>
          api.saveTriggerConfig({
            document: {
              agent_did: deployment.agentDid,
              trigger_id,
              display_name: "New trigger",
              task_id: task.taskId,
              source: { kind: "schedule", schedule_id: schedule.schedule_id },
              enabled: false,
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "triggers",
          item: trigger_id,
        });
      }}
      detail={(id) => {
        const trigger = deployment.triggers.find((t) => t.config.trigger_id === id)!;
        return (
          <Editor
            key={trigger.config.trigger_id}
            shell={shell}
            deployment={deployment}
            trigger={trigger}
          />
        );
      }}
    />
  );
}
