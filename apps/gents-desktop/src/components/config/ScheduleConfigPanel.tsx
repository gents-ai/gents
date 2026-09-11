import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  Schedule,
  ScheduleDeleteRequest,
  ScheduleSaveRequest,
  TaskRunResult,
} from "@source-inc/gents-desktop-client";
import type { ScheduleCadence } from "@source-inc/gents-desktop-client/generated/ScheduleCadence";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import {
  ignoreHandledActionError,
  isOptionalInt,
  linesToArray,
  parseOptionalInt,
} from "./formUtils";

export type ScheduleConfigPanelProps = {
  deployment: DeploymentView;
  selectedScheduleId: string | null;
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
        (schedule) => schedule.schedule_id === selectedScheduleId,
      ) ?? null,
    [deployment.schedules, selectedScheduleId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Automation"
        items={deployment.schedules.map((schedule) => ({
          id: schedule.schedule_id,
          title: schedule.display_name ?? schedule.schedule_id,
          meta: describeCadence(schedule.cadence),
        }))}
        selectedId={selectedScheduleId}
        testPrefix="schedule"
        title="Schedules"
        onCreate={onCreateSchedule}
        onSelect={onSelectSchedule}
      />

      <ScheduleConfigEditor
        agentDid={deployment.agentDid}
        runningTask={runningTask}
        savedStatus={savedStatus}
        saving={saving}
        schedule={selectedSchedule}
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
  schedule: Schedule | null;
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
      await onDeleteScheduleConfig({ scheduleId: schedule.schedule_id, agentDid });
      onDeleted();
    } catch {}
  }

  const [scheduleId, setScheduleId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [cadenceKind, setCadenceKind] = useState<"interval" | "cron">("interval");
  const [intervalSecs, setIntervalSecs] = useState("");
  const [cronExpression, setCronExpression] = useState("");
  const [cronTimezone, setCronTimezone] = useState("UTC");
  const [tags, setTags] = useState("");
  const [runStatus, setRunStatus] = useState<TaskRunResult | null>(null);

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const baseline = scheduleFormValues(schedule);
    setScheduleId(baseline.scheduleId);
    setDisplayName(baseline.displayName);
    setCadenceKind(baseline.cadenceKind);
    setIntervalSecs(baseline.intervalSecs);
    setCronExpression(baseline.cronExpression);
    setCronTimezone(baseline.cronTimezone);
    setTags(baseline.tags);
    setSaveError(null);
  }, [schedule?.schedule_id, schedule?.updated_at]);

  useEffect(() => {
    setRunStatus(null);
  }, [schedule?.schedule_id]);

  const intervalValid = isOptionalInt(intervalSecs, { min: 1 });
  const cronValid = cadenceKind !== "cron" || cronExpression.trim().length > 0;
  const cadenceValid = cadenceKind === "interval" ? intervalValid : cronValid;

  async function submitSchedule(event: FormEvent) {
    event.preventDefault();
    if (!scheduleId.trim()) {
      return;
    }
    const cadence: ScheduleCadence | null =
      cadenceKind === "interval"
        ? parseOptionalInt(intervalSecs) != null
          ? { kind: "interval", interval_secs: parseOptionalInt(intervalSecs)! }
          : null
        : cronExpression.trim()
          ? { kind: "cron", expression: cronExpression.trim(), timezone: cronTimezone }
          : null;
    if (cadence == null) {
      return;
    }
    const document: Schedule = {
      agent_did: agentDid,
      schedule_id: scheduleId.trim(),
      display_name: optionalTrimmed(displayName),
      cadence,
      tags: linesToArray(tags),
    };
    try {
      await onSaveScheduleConfig({ document });
      onSaved(document.schedule_id);
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
          {
            scheduleId,
            displayName,
            cadenceKind,
            intervalSecs,
            cronExpression,
            cronTimezone,
            tags,
          },
          scheduleFormValues(schedule),
        )}
        eyebrow="Schedule"
        saved={savedStatus === `schedule:${scheduleId.trim()}`}
        title={displayName || scheduleId || "New Schedule"}
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
          <span>Display name</span>
          <input
            data-testid="schedule-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Cadence</span>
          <select
            data-testid="schedule-cadence-kind"
            onChange={(event) =>
              setCadenceKind(event.currentTarget.value as "interval" | "cron")
            }
            value={cadenceKind}
          >
            <option value="interval">Interval</option>
            <option value="cron">Cron</option>
          </select>
        </label>
        {cadenceKind === "interval" ? (
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
        ) : (
          <label className="field">
            <span>Cron expression</span>
            <input
              data-testid="schedule-cron-expression"
              onChange={(event) => setCronExpression(event.currentTarget.value)}
              value={cronExpression}
            />
          </label>
        )}
        {cadenceKind === "cron" ? (
          <label className="field">
            <span>Timezone</span>
            <input
              data-testid="schedule-cron-timezone"
              onChange={(event) => setCronTimezone(event.currentTarget.value)}
              value={cronTimezone}
            />
          </label>
        ) : null}
      </div>
      <label className="field">
        <span>Tags</span>
        <textarea
          className="config-small-textarea"
          data-testid="schedule-tags"
          onChange={(event) => setTags(event.currentTarget.value)}
          placeholder="One tag per line"
          value={tags}
        />
      </label>
      {schedule ? (
        <div className="facts">
          <div>
            <dt>Created</dt>
            <dd>{schedule.created_at ?? "unknown"}</dd>
          </div>
          <div>
            <dt>Updated</dt>
            <dd>{schedule.updated_at ?? "unknown"}</dd>
          </div>
        </div>
      ) : null}
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
          message={`Delete schedule "${schedule?.schedule_id ?? ""}"? Triggers referencing it stop firing.`}
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
          disabled={saving || !scheduleId.trim() || !cadenceValid}
          type="submit"
        >
          {saving ? "Saving..." : "Save Schedule"}
        </button>
      </div>

      <section className="config-runner">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Manual Run</p>
            <h3>{displayName || scheduleId || "Schedule"}</h3>
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
            {runningTask ? "Running..." : "Run Schedule Now"}
          </button>
        </div>
      </section>
    </form>
  );
}

function scheduleFormValues(schedule: Schedule | null) {
  const cadence = schedule?.cadence;
  return {
    scheduleId: schedule?.schedule_id ?? "",
    displayName: schedule?.display_name ?? "",
    cadenceKind: cadence?.kind === "cron" ? ("cron" as const) : ("interval" as const),
    intervalSecs: cadence?.kind === "interval" ? String(cadence.interval_secs) : "",
    cronExpression: cadence?.kind === "cron" ? cadence.expression : "",
    cronTimezone: cadence?.kind === "cron" ? cadence.timezone : "UTC",
    tags: schedule?.tags?.length ? schedule.tags.join("\n") : "",
  };
}

function optionalTrimmed(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function describeCadence(cadence: ScheduleCadence): string {
  if (cadence.kind === "interval") {
    return `every ${cadence.interval_secs}s`;
  }
  return `${cadence.expression} (${cadence.timezone})`;
}
