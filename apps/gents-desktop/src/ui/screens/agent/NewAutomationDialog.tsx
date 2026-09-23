/* One dialog for "make it run on its own": when, what, who. Nothing is
   written until Create; then the schedule or event source, the task and
   the trigger land together in one config component apply (one
   transaction), with the same ids on a retry, so a failure leaves no half
   of it behind. A new trigger starts off unless the person turns it on
   here. Existing schedules, sources and tasks can be reused instead of
   made. */
import { useRef, useState } from "react";
import type {
  ConfigComponentsApplyRequest,
  DeploymentView,
  EventSource,
  Schedule,
  Trigger,
} from "@source-inc/gents-desktop-client";

export type Task = NonNullable<
  ConfigComponentsApplyRequest["document"]["tasks"]
>[number];
type TriggerSource = Trigger["source"];
import { Button } from "@gents/ui/components/button";
import { Checkbox } from "@gents/ui/components/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@gents/ui/components/field";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Textarea } from "@gents/ui/components/textarea";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import { cadenceInWords, eventInWords } from "./automation";
import { newId, validateCronSchedule } from "./draft";

const NEW = "new:";
export const TZ = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

/* the cadences people ask for; Custom takes a cron expression */
export const PRESETS = [
  { id: "daily", label: "Every day at 09:00", cron: "0 9 * * *" },
  { id: "weekdays", label: "Every weekday at 09:00", cron: "0 9 * * 1-5" },
  { id: "weekly", label: "Every Monday at 09:00", cron: "0 9 * * 1" },
  { id: "hourly", label: "Every hour", cron: "0 * * * *" },
  { id: "15min", label: "Every 15 minutes", cron: "*/15 * * * *" },
  { id: "custom", label: "Custom cron…", cron: "" },
] as const;

function Choice({
  id,
  label,
  value,
  onChange,
  items,
  newLabel,
  description,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  items: { value: string; label: string }[];
  newLabel: string;
  description?: string;
}) {
  const all = [...items, { value: NEW, label: newLabel }];
  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Select items={all} value={value} onValueChange={(v) => v && onChange(v)}>
        <SelectTrigger id={id} className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {items.map((i) => (
            <SelectItem key={i.value} value={i.value}>
              {i.label}
            </SelectItem>
          ))}
          {items.length > 0 && <SelectSeparator />}
          <SelectItem value={NEW}>{newLabel}</SelectItem>
        </SelectContent>
      </Select>
      {description && <FieldDescription>{description}</FieldDescription>}
    </Field>
  );
}

export function NewAutomationDialog({
  shell,
  deployment,
  open,
  onOpenChange,
  forTask = false,
  task,
  initialKind,
  onCreated,
}: {
  shell: Shell;
  deployment: DeploymentView;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /* from Tasks: always a new task, which may run manually; lands on the task */
  forTask?: boolean;
  /* from a task's page: a trigger for that task, which stays where it is */
  task?: string;
  initialKind?: "schedule" | "event";
  /* the new trigger's id, for a caller that stays on its page */
  onCreated?: (triggerId: string) => void;
}) {
  const defaultBehavior =
    deployment.behaviors.find((b) => b.isDefault)?.behaviorId ??
    deployment.behaviors[0]?.behaviorId ??
    "";
  const [name, setName] = useState("");
  const startKind = forTask ? "manual" : (initialKind ?? "schedule");
  const [kind, setKind] = useState<"schedule" | "event" | "manual">(startKind);
  const [scheduleId, setScheduleId] = useState(NEW);
  const [preset, setPreset] = useState<(typeof PRESETS)[number]["id"]>("daily");
  const [cron, setCron] = useState("");
  const [eventId, setEventId] = useState(NEW);
  /* no default collection: watching a collection the task itself writes to
     (AgentRequest) would let it trigger itself, so the watch is a choice */
  const [collection, setCollection] = useState("");
  const [eventKind, setEventKind] = useState("created");
  const [taskId, setTaskId] = useState(task ?? NEW);
  const [enable, setEnable] = useState(false);
  /* the ids of what Create writes, kept across a failed attempt */
  const ids = useRef<Record<string, string>>({});
  const idFor = (key: string, prefix: string) => (ids.current[key] ??= newId(prefix));
  const [prompt, setPrompt] = useState("");
  const [behaviorId, setBehaviorId] = useState(defaultBehavior);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const close = (next: boolean) => {
    if (busy) return;
    if (!next) {
      setName("");
      setKind(startKind);
      setEnable(false);
      ids.current = {};
      setScheduleId(NEW);
      setPreset("daily");
      setCron("");
      setEventId(NEW);
      setCollection("");
      setEventKind("created");
      setTaskId(task ?? NEW);
      setPrompt("");
      setBehaviorId(defaultBehavior);
      setError(null);
    }
    onOpenChange(next);
  };

  /* the behavior that will run it: the new task's pick, or the chosen task's */
  const runningBehaviorId =
    taskId === NEW
      ? behaviorId
      : deployment.tasks.find((t) => t.taskId === taskId)?.behaviorId;
  const behavior =
    deployment.behaviors.find((b) => b.behaviorId === runningBehaviorId) ?? null;
  const behaviorOff = behavior !== null && !behavior.enabled;

  const create = async () => {
    setError(null);
    if (taskId === NEW && !prompt.trim()) {
      setError("Say what the agent should do.");
      return;
    }
    if (taskId === NEW && !behaviorId) {
      setError("Choose the behavior that runs it.");
      return;
    }
    setBusy(true);
    try {
      const agent_did = deployment.agentDid;
      const trigger_id = idFor("trigger", "trig");
      const displayName =
        name.trim() ||
        (taskId === NEW
          ? prompt.trim().split("\n")[0].slice(0, 60)
          : (deployment.tasks.find((t) => t.taskId === taskId)?.name ?? "Automation"));
      const schedules: Schedule[] = [];
      const eventSources: EventSource[] = [];
      const tasks: Task[] = [];
      const triggers: Trigger[] = [];

      /* when */
      let source: TriggerSource | null = null;
      if (kind === "schedule") {
        let schedule_id = scheduleId;
        if (scheduleId === NEW) {
          schedule_id = idFor("schedule", "sched");
          const expression =
            preset === "custom" ? cron : PRESETS.find((p) => p.id === preset)!.cron;
          const cadence = {
            kind: "cron" as const,
            ...validateCronSchedule(expression, TZ),
            missed_run_policy: "latest_only" as const,
          };
          schedules.push({
            agent_did,
            schedule_id,
            display_name: cadenceInWords({ agent_did, schedule_id, cadence }),
            cadence,
          });
        }
        source = { kind: "schedule", schedule_id };
      } else if (kind === "event") {
        let event_source_id = eventId;
        if (eventId === NEW) {
          if (!collection.trim()) throw new Error("Name the collection to watch.");
          event_source_id = idFor("event", "evsrc");
          const doc = {
            agent_did,
            event_source_id,
            display_name: null as string | null,
            source_collection: collection.trim(),
            event_kind: eventKind.trim() || null,
          };
          doc.display_name = eventInWords(doc);
          eventSources.push(doc);
        }
        source = { kind: "event", event_source_id };
      }

      /* what */
      let task_id = taskId;
      if (taskId === NEW) {
        task_id = idFor("task", "task");
        tasks.push({
          agent_did,
          task_id,
          display_name: displayName,
          description: null,
          behavior_id: behaviorId,
          prompt_template: prompt.trim(),
          goal_objective_template: null,
          goal_token_budget: null,
          enabled: true,
          output_schema_ref: null,
        });
      }

      /* the trigger: off unless turned on here, and never on for a behavior that is off */
      if (source)
        triggers.push({
          agent_did,
          trigger_id,
          display_name: displayName,
          task_id,
          source,
          enabled: enable && !behaviorOff,
          concurrency: "serial",
        });
      await shell.applyConfig((api) =>
        api.applyConfigComponents({
          document: {
            agent_principal: { agent_did },
            ...(schedules.length ? { schedules } : {}),
            ...(eventSources.length ? { event_sources: eventSources } : {}),
            ...(tasks.length ? { tasks } : {}),
            ...(triggers.length ? { triggers } : {}),
          },
        }),
      );
      close(false);
      if (onCreated && source) onCreated(trigger_id);
      else
        navigate(
          forTask
            ? { name: "agent", agentDid: agent_did, section: "tasks", item: task_id }
            : {
                name: "agent",
                agentDid: agent_did,
                section: "triggers",
                item: trigger_id,
              },
        );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent aria-modal="true" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {forTask ? "New task" : task ? "When it runs" : "New trigger"}
          </DialogTitle>
          <DialogDescription>
            When it runs, what it does, and which behavior does it. Everything else can
            be tuned afterwards.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="auto-name">Name</FieldLabel>
            <Input
              id="auto-name"
              autoFocus
              placeholder="Morning mailbox digest"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </Field>

          <Field>
            <FieldLabel htmlFor="auto-kind">When</FieldLabel>
            <Select
              items={[
                ...(forTask ? [{ value: "manual", label: "When I run it" }] : []),
                { value: "schedule", label: "On a schedule" },
                { value: "event", label: "When something happens" },
              ]}
              value={kind}
              onValueChange={(v) => v && setKind(v as "schedule" | "event" | "manual")}
            >
              <SelectTrigger id="auto-kind" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {forTask && <SelectItem value="manual">When I run it</SelectItem>}
                <SelectItem value="schedule">On a schedule</SelectItem>
                <SelectItem value="event">When something happens</SelectItem>
              </SelectContent>
            </Select>
          </Field>

          {kind === "manual" ? null : kind === "schedule" ? (
            <>
              <Choice
                id="auto-schedule"
                label="Schedule"
                value={scheduleId}
                onChange={setScheduleId}
                items={deployment.schedules.map((s) => ({
                  value: s.schedule_id,
                  label: s.display_name ?? cadenceInWords(s),
                }))}
                newLabel="New schedule…"
              />
              {scheduleId === NEW && (
                <Field>
                  <FieldLabel htmlFor="auto-preset">Cadence</FieldLabel>
                  <Select
                    items={PRESETS.map((p) => ({ value: p.id, label: p.label }))}
                    value={preset}
                    onValueChange={(v) => v && setPreset(v as typeof preset)}
                  >
                    <SelectTrigger id="auto-preset" className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {PRESETS.map((p) => (
                        <SelectItem key={p.id} value={p.id}>
                          {p.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {preset === "custom" && (
                    <Input
                      aria-label="Cron expression"
                      className="mt-2 font-mono"
                      placeholder="0 9 * * 1-5"
                      value={cron}
                      onChange={(e) => setCron(e.target.value)}
                    />
                  )}
                  <FieldDescription>Times are in {TZ}.</FieldDescription>
                </Field>
              )}
            </>
          ) : (
            <>
              <Choice
                id="auto-event"
                label="Event"
                value={eventId}
                onChange={setEventId}
                items={deployment.eventSources.map((e) => ({
                  value: e.event_source_id,
                  label: e.display_name ?? eventInWords(e),
                }))}
                newLabel="New event source…"
              />
              {eventId === NEW && (
                <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
                  <Field>
                    <FieldLabel htmlFor="auto-collection">Collection</FieldLabel>
                    <Input
                      id="auto-collection"
                      className="font-mono"
                      placeholder="MailboxItem"
                      value={collection}
                      onChange={(e) => setCollection(e.target.value)}
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="auto-event-kind">Event kind</FieldLabel>
                    <Input
                      id="auto-event-kind"
                      className="font-mono"
                      value={eventKind}
                      onChange={(e) => setEventKind(e.target.value)}
                    />
                  </Field>
                </div>
              )}
            </>
          )}

          {!forTask && !task && (
            <Choice
              id="auto-task"
              label="What"
              value={taskId}
              onChange={setTaskId}
              items={deployment.tasks.map((t) => ({
                value: t.taskId,
                label: t.name ?? t.taskId,
              }))}
              newLabel="New task…"
            />
          )}
          {taskId === NEW && (
            <>
              <Field>
                <FieldLabel htmlFor="auto-prompt">Prompt</FieldLabel>
                <Textarea
                  id="auto-prompt"
                  rows={4}
                  placeholder="Read the mailbox items from the last day and post a short digest."
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="auto-behavior">Who</FieldLabel>
                <Select
                  items={deployment.behaviors.map((b) => ({
                    value: b.behaviorId,
                    label: b.displayName,
                  }))}
                  value={behaviorId}
                  onValueChange={(v) => v && setBehaviorId(v)}
                >
                  <SelectTrigger id="auto-behavior" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {deployment.behaviors.map((b) => (
                      <SelectItem key={b.behaviorId} value={b.behaviorId}>
                        {b.displayName}
                        {b.isDefault ? " (default)" : ""}
                        {b.enabled ? "" : " · off"}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {behaviorOff && (
                  <FieldDescription>
                    {behavior.displayName} is off, so the automation is created off too.
                  </FieldDescription>
                )}
              </Field>
            </>
          )}
          {kind !== "manual" && (
            <div className="flex items-center gap-2 text-sm">
              <Checkbox
                id="auto-enable"
                checked={enable && !behaviorOff}
                disabled={behaviorOff}
                onCheckedChange={(v) => setEnable(Boolean(v))}
              />
              <label htmlFor="auto-enable">Turn it on now</label>
            </div>
          )}
          {error && <FieldError>{error}</FieldError>}
        </div>
        <DialogFooter>
          <Button variant="ghost" disabled={busy} onClick={() => close(false)}>
            Cancel
          </Button>
          <Button variant="brand" disabled={busy} onClick={create}>
            {busy ? "Creating…" : "Create"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
