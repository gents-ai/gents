import { useState } from "react";
import { toast } from "sonner";
import type { DeploymentView, Schedule } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  TextRow,
} from "./editors";
import {
  fromLinesOrNull,
  newId,
  optionalInteger,
  str,
  toLines,
  useDraft,
  validateCronSchedule,
} from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";

function cadenceLabel(s: Schedule) {
  return s.cadence.kind === "cron"
    ? s.cadence.expression
    : `every ${s.cadence.interval_secs}s`;
}

function Editor({
  shell,
  deployment,
  schedule,
}: {
  shell: Shell;
  deployment: DeploymentView;
  schedule: Schedule;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "schedules",
  };
  const saved = {
    displayName: schedule.display_name ?? "",
    cadenceKind: schedule.cadence.kind,
    intervalSecs:
      schedule.cadence.kind === "interval" ? str(schedule.cadence.interval_secs) : "",
    expression: schedule.cadence.kind === "cron" ? schedule.cadence.expression : "",
    timezone: schedule.cadence.kind === "cron" ? schedule.cadence.timezone : "UTC",
    tags: toLines(schedule.tags ?? []),
  };
  const d = useDraft(saved, async (next) => {
    const cadence =
      next.cadenceKind === "interval"
        ? {
            kind: "interval" as const,
            interval_secs:
              optionalInteger("Interval seconds", next.intervalSecs, { min: 1 }) ??
              (() => {
                throw new Error("Interval seconds is required");
              })(),
          }
        : {
            kind: "cron" as const,
            ...validateCronSchedule(next.expression, next.timezone),
            missed_run_policy: "latest_only" as const,
          };
    await shell.applyConfig((api) =>
      api.saveScheduleConfig({
        document: {
          ...schedule,
          display_name: next.displayName.trim() || null,
          cadence,
          tags: fromLinesOrNull(next.tags),
        },
      }),
    );
  });
  const [lastRun, setLastRun] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const id = (f: string) => `${schedule.schedule_id}-${f}`;
  const run = async () => {
    setRunning(true);
    const pending = shell.runSchedule({ scheduleId: schedule.schedule_id });
    const intentGeneration = shell.captureComposeIntent();
    try {
      const result = await pending;
      if (!shell.acceptsComposeIntent(intentGeneration)) return;
      setLastRun(result.requestId);
      toast(`Schedule started · ${result.requestId}`);
    } catch (error) {
      if (!shell.acceptsComposeIntent(intentGeneration)) return;
      toast(
        `Schedule failed to start: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setRunning(false);
    }
  };
  return (
    <>
      <Group title={schedule.display_name ?? schedule.schedule_id}>
        <FactRow label="Schedule ID" mono>
          {schedule.schedule_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("cadence")}
          label="Cadence"
          value={d.draft.cadenceKind}
          onChange={(v) => d.choose("cadenceKind", v as "interval" | "cron")}
          items={[
            { value: "interval", label: "Interval" },
            { value: "cron", label: "Cron" },
          ]}
        />
        {d.draft.cadenceKind === "interval" ? (
          <NumberRow
            id={id("interval")}
            label="Interval seconds"
            description="Positive whole number."
            value={d.draft.intervalSecs}
            onChange={(v) => d.set("intervalSecs", v)}
            onCommit={d.commit}
            onEnter={d.onEnter}
          />
        ) : (
          <>
            <TextRow
              id={id("cron")}
              label="Cron"
              value={d.draft.expression}
              onChange={(v) => d.set("expression", v)}
              onCommit={d.commit}
              onEnter={d.onEnter}
            />
            <TextRow
              id={id("tz")}
              label="Timezone"
              value={d.draft.timezone}
              onChange={(v) => d.set("timezone", v)}
              onCommit={d.commit}
              onEnter={d.onEnter}
            />
          </>
        )}
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
      <Group
        title="Manual run"
        action={
          <Button size="sm" variant="brand" disabled={running} onClick={run}>
            {running ? "Starting…" : "Run schedule now"}
          </Button>
        }
      >
        <FactRow label="Trigger">
          Uses the enabled trigger currently bound to this schedule.
        </FactRow>
        {lastRun && (
          <FactRow label="Started" mono>
            {lastRun}
          </FactRow>
        )}
      </Group>
      <DeleteButton
        label={schedule.display_name ?? schedule.schedule_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteScheduleConfig({
              scheduleId: schedule.schedule_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function SchedulesPanel({
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
    section: "schedules",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.schedules.map((s) => ({
        id: s.schedule_id,
        title: s.display_name ?? s.schedule_id,
        meta: cadenceLabel(s),
      }))}
      createLabel="New schedule"
      empty="No schedules. A trigger binds a task to a schedule."
      onCreate={async () => {
        const schedule_id = newId("sched");
        await shell.applyConfig((api) =>
          api.saveScheduleConfig({
            document: {
              agent_did: deployment.agentDid,
              schedule_id,
              display_name: "New schedule",
              cadence: { kind: "cron", expression: "0 * * * *", timezone: "UTC" },
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "schedules",
          item: schedule_id,
        });
      }}
      detail={(id) => {
        const schedule = deployment.schedules.find((s) => s.schedule_id === id)!;
        return (
          <Editor
            key={schedule.schedule_id}
            shell={shell}
            deployment={deployment}
            schedule={schedule}
          />
        );
      }}
    />
  );
}
