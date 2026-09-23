import { useState } from "react";
import { dependentsWarning } from "./dependents";
import { toast } from "sonner";
import type { DeploymentView, Schedule } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  TextRow,
  TagsRow,
} from "./editors";
import { newId, optionalInteger, str, useDraft, validateCronSchedule } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";

function cadenceLabel(s: Schedule) {
  return s.cadence.kind === "cron"
    ? s.cadence.expression
    : `every ${s.cadence.interval_secs}s`;
}

export function ScheduleEditor({
  shell,
  deployment,
  schedule,
  embedded = false,
}: {
  shell: Shell;
  deployment: DeploymentView;
  schedule: Schedule;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
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
    tags: schedule.tags ?? [],
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
          tags: next.tags.length ? next.tags : null,
        },
      }),
    );
  });
  const [lastRun, setLastRun] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const id = (f: string) => `${schedule.schedule_id}-${f}`;
  const run = async () => {
    setRunning(true);
    const pending = shell.runSchedule({
      scheduleId: schedule.schedule_id,
      agentDid: schedule.agent_did,
    });
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
      <Group
        title={embedded ? undefined : (schedule.display_name ?? schedule.schedule_id)}
      >
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
      {!embedded && (
        <DeleteButton
          label={schedule.display_name ?? schedule.schedule_id}
          warning={dependentsWarning(deployment, "schedule", schedule.schedule_id)}
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
      )}
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
        tags: s.tags,
        trailing: (
          <RowMenu
            name={s.display_name ?? s.schedule_id}
            base={base}
            id={s.schedule_id}
            onDuplicate={async () => {
              const schedule_id = newId("sched");
              await shell.applyConfig((api) =>
                api.saveScheduleConfig({
                  document: {
                    ...s,
                    schedule_id,
                    display_name: `${s.display_name ?? s.schedule_id} copy`,
                    created_at: null,
                    updated_at: null,
                  },
                }),
              );
              return schedule_id;
            }}
            onDelete={() =>
              shell.applyConfig((api) =>
                api.deleteScheduleConfig({
                  scheduleId: s.schedule_id,
                  agentDid: deployment.agentDid,
                }),
              )
            }
            warning={(() => {
              const n = deployment.triggers.filter(
                (x) =>
                  x.config.source.kind === "schedule" &&
                  x.config.source.schedule_id === s.schedule_id,
              ).length;
              return n
                ? `${n} ${n === 1 ? "automation uses" : "automations use"} it.`
                : undefined;
            })()}
          />
        ),
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
          <ScheduleEditor
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
