import type { NodeView } from "../../../hooks/fleetStore";
import { useState } from "react";
import { dependentsWarning } from "./dependents";
import { toast } from "sonner";
import type { Schedule } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { navigate } from "@/lib/router";
import {
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  TextRow,
  TagsRow,
} from "./editors";
import {
  newId,
  problemOf,
  requiredInteger,
  str,
  useDraft,
  validateCronExpression,
  validateCronSchedule,
  validateTimezone,
} from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";
import { toastFailure } from "@/lib/failure";
import { useApp } from "@/app/AppContext";

const INTERVAL = "Interval seconds";

function cadenceLabel(s: Schedule) {
  return s.cadence.kind === "cron"
    ? s.cadence.expression
    : `every ${s.cadence.interval_secs}s`;
}

export function ScheduleEditor({
  deployment,
  schedule,
  embedded = false,
}: {
  deployment: NodeView;
  schedule: Schedule;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const {
    actions: { acceptsComposeIntent, captureComposeIntent, runSchedule, changeConfig },
  } = useApp();
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
  const d = useDraft(
    saved,
    async (next) => {
      const cadence =
        next.cadenceKind === "interval"
          ? {
              kind: "interval" as const,
              interval_secs: requiredInteger(INTERVAL, next.intervalSecs, { min: 1 }),
            }
          : {
              kind: "cron" as const,
              ...validateCronSchedule(next.expression, next.timezone),
              missed_run_policy: "latest_only" as const,
            };
      await changeConfig("saveScheduleConfig", {
        document: {
          ...schedule,
          display_name: next.displayName.trim() || null,
          cadence,
          tags: next.tags.length ? next.tags : null,
        },
      });
    },
    {
      /* only the cadence chosen is checked */
      problems: (next) =>
        next.cadenceKind === "interval"
          ? {
              intervalSecs: problemOf(() =>
                requiredInteger(INTERVAL, next.intervalSecs, { min: 1 }),
              ),
            }
          : {
              expression: problemOf(() => validateCronExpression(next.expression)),
              timezone: problemOf(() => validateTimezone(next.timezone)),
            },
    },
  );
  const [lastRun, setLastRun] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const id = (f: string) => `${schedule.schedule_id}-${f}`;
  const run = async () => {
    setRunning(true);
    const pending = runSchedule({
      scheduleId: schedule.schedule_id,
      nodeDid: schedule.node_did,
    });
    const intentGeneration = captureComposeIntent();
    try {
      const result = await pending;
      if (!acceptsComposeIntent(intentGeneration)) return;
      setLastRun(result.requestId);
      toast(`Schedule started · ${result.requestId}`);
    } catch (error) {
      if (!acceptsComposeIntent(intentGeneration)) return;
      toastFailure("start the schedule", error);
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
        />
        <ChoiceRow
          id={id("cadence")}
          label="Cadence"
          value={d.draft.cadenceKind}
          onChange={(v) => d.set("cadenceKind", v as "interval" | "cron")}
          items={[
            { value: "interval", label: "Interval" },
            { value: "cron", label: "Cron" },
          ]}
        />
        {d.draft.cadenceKind === "interval" ? (
          <NumberRow
            id={id("interval")}
            label={INTERVAL}
            description="Positive whole number."
            value={d.draft.intervalSecs}
            onChange={(v) => d.set("intervalSecs", v)}
            error={d.problems.intervalSecs}
          />
        ) : (
          <>
            <TextRow
              id={id("cron")}
              label="Cron"
              value={d.draft.expression}
              onChange={(v) => d.set("expression", v)}
              error={d.problems.expression}
            />
            <TextRow
              id={id("tz")}
              label="Timezone"
              value={d.draft.timezone}
              onChange={(v) => d.set("timezone", v)}
              error={d.problems.timezone}
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
        draft={d}
        fields={{
          intervalSecs: id("interval"),
          expression: id("cron"),
          timezone: id("tz"),
        }}
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
            changeConfig("deleteScheduleConfig", {
              scheduleId: schedule.schedule_id,
              nodeDid: deployment.nodeDid,
            })
          }
        />
      )}
    </>
  );
}

export function SchedulesPanel({
  deployment,
  item,
}: {
  deployment: NodeView;
  item?: string;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
              await changeConfig("saveScheduleConfig", {
                document: {
                  ...s,
                  schedule_id,
                  display_name: `${s.display_name ?? s.schedule_id} copy`,
                  created_at: null,
                  updated_at: null,
                },
              });
              return schedule_id;
            }}
            onDelete={() =>
              changeConfig("deleteScheduleConfig", {
                scheduleId: s.schedule_id,
                nodeDid: deployment.nodeDid,
              })
            }
            warning={dependentsWarning(deployment, "schedule", s.schedule_id)}
          />
        ),
      }))}
      createLabel="New schedule"
      empty="No schedules. A trigger binds a task to a schedule."
      onCreate={async () => {
        const schedule_id = newId("sched");
        await changeConfig("saveScheduleConfig", {
          document: {
            node_did: deployment.nodeDid,
            schedule_id,
            display_name: "New schedule",
            cadence: { kind: "cron", expression: "0 * * * *", timezone: "UTC" },
          },
        });
        navigate({
          name: "agent",
          nodeDid: deployment.nodeDid,
          section: "schedules",
          item: schedule_id,
        });
      }}
      detail={(id) => {
        const schedule = deployment.schedules.find((s) => s.schedule_id === id)!;
        return (
          <ScheduleEditor
            key={schedule.schedule_id}

            deployment={deployment}
            schedule={schedule}
          />
        );
      }}
    />
  );
}
