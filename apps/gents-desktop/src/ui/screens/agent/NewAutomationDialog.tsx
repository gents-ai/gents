import type { NodeView } from "../../../hooks/fleetStore";
import { agentReadiness } from "@/lib/agent-readiness";
import { useEffect, useRef, useState } from "react";
import { Hand, Timer, Zap } from "lucide-react";
import type {
  ConfigComponentsApplyRequest,
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
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { Field, FieldError, FieldLabel } from "@gents/ui/components/field";
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
import { cn } from "@gents/ui/lib/utils";
import { navigate } from "@/lib/router";
import { cadenceInWords, eventInWords } from "./automation";
import { newId, validateCronSchedule } from "./draft";
import { defaultAgentOf } from "@/lib/agents";
import { agentOf } from "@/lib/agents";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";
import { AgentPicker } from "../BehaviorPicker";
import { CollectionPicker, OTHER } from "./CollectionPicker";

const NEW = "new:";
export const TZ = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

export const PRESETS = [
  { id: "daily", label: "Every day at 09:00", cron: "0 9 * * *" },
  { id: "weekdays", label: "Every weekday at 09:00", cron: "0 9 * * 1-5" },
  { id: "weekly", label: "Every Monday at 09:00", cron: "0 9 * * 1" },
  { id: "hourly", label: "Every hour", cron: "0 * * * *" },
  { id: "15min", label: "Every 15 minutes", cron: "*/15 * * * *" },
  { id: "custom", label: "Custom cron…", cron: "" },
] as const;

type Preset = (typeof PRESETS)[number]["id"];
type Kind = "schedule" | "event" | "manual";

/** A starting point: its name, schedule and prompt fill the form. */
export type AutomationStart = {
  name: string;
  preset: Preset;
  prompt: string;
};

function Choice({
  id,
  label,
  value,
  onChange,
  items,
  newLabel,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  items: { value: string; label: string }[];
  newLabel: string;
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
    </Field>
  );
}

const KINDS: {
  id: Kind;
  label: string;
  hint: string;
  icon: typeof Timer;
}[] = [
  {
    id: "schedule",
    label: "On a schedule",
    hint: "Every day, hour or week",
    icon: Timer,
  },
  {
    id: "event",
    label: "When something happens",
    hint: "A new document arrives",
    icon: Zap,
  },
  {
    id: "manual",
    label: "Only when I start it",
    hint: "Run it from its page",
    icon: Hand,
  },
];

/* how often, short enough for a chip */
const SHORT: Record<Preset, string> = {
  "15min": "Every 15 minutes",
  hourly: "Hourly",
  daily: "Daily · 9:00",
  weekdays: "Weekdays · 9:00",
  weekly: "Mondays · 9:00",
  custom: "Custom…",
};

/** What a new automation is, shaped like the composer: its name, what the
    agent should do, and chips for when it runs and who runs it. It makes
    its own schedule and task unless existing ones are chosen. */
export function AutomationForm({
  deployment,
  forTask = false,
  task,
  initialKind,
  start,
  layout,
  onCancel,
  onCreated,
  onCreatedTask,
}: {
  deployment: NodeView;
  forTask?: boolean;
  /** an existing task this adds a trigger to */
  task?: string;
  initialKind?: "schedule" | "event";
  start?: AutomationStart | null;
  layout: "page" | "dialog";
  onCancel: () => void;
  onCreated?: (triggerId: string) => void;
  /** with no trigger (run by hand), the task made */
  onCreatedTask?: (taskId: string) => void;
}) {
  const { changeConfig } = useApp().actions;
  const defaultAgent =
    defaultAgentOf(deployment)?.agentId ?? deployment.agents[0]?.agentId ?? "";
  const [name, setName] = useState(start?.name ?? "");
  const [kind, setKind] = useState<Kind | null>(
    start ? "schedule" : forTask ? "manual" : (initialKind ?? null),
  );
  const [scheduleId, setScheduleId] = useState(NEW);
  const [preset, setPreset] = useState<Preset | null>(start?.preset ?? null);
  const [cron, setCron] = useState("");
  const [eventId, setEventId] = useState(NEW);
  // The watched collection must be explicit to avoid self-triggering task writes.
  const [watch, setWatch] = useState("");
  const [collection, setCollection] = useState("");
  const [taskId, setTaskId] = useState(task ?? NEW);
  const [enable, setEnable] = useState(false);
  const [reuse, setReuse] = useState(false);
  const ids = useRef<Record<string, string>>({});
  const idFor = (key: string, prefix: string) => (ids.current[key] ??= newId(prefix));
  const [prompt, setPrompt] = useState(start?.prompt ?? "");
  const [agentId, setAgentId] = useState(defaultAgent);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const existingTask =
    taskId === NEW ? null : deployment.tasks.find((t) => t.taskId === taskId);
  const runningAgentId = taskId === NEW ? agentId : existingTask?.agentId;
  const agent = agentOf(deployment, runningAgentId) ?? null;
  const readiness = agent ? agentReadiness(deployment, agent.agentId) : null;
  const agentOff = readiness !== null && !readiness.ready;
  const fromPrompt = prompt.trim().split("\n")[0].slice(0, 60);

  const canReuse =
    !forTask &&
    (deployment.schedules.length > 0 ||
      deployment.eventSources.length > 0 ||
      (!task && deployment.tasks.length > 0));

  // an answer to the last refusal clears it
  useEffect(
    () => setError(null),
    [prompt, kind, preset, watch, collection, agentId, taskId],
  );

  const create = async () => {
    setError(null);
    if (!kind) {
      setError("Choose when it runs.");
      return;
    }
    if (kind === "schedule" && scheduleId === NEW && !preset) {
      setError("Choose how often it runs.");
      return;
    }
    if (taskId === NEW && !prompt.trim()) {
      setError("Say what the agent should do.");
      return;
    }
    if (taskId === NEW && !agentId) {
      setError("Choose the agent that runs it.");
      return;
    }
    setBusy(true);
    try {
      const node_did = deployment.nodeDid;
      const trigger_id = idFor("trigger", "trig");
      const displayName =
        name.trim() ||
        (taskId === NEW ? fromPrompt : (existingTask?.name ?? "Automation"));
      const schedules: Schedule[] = [];
      const eventSources: EventSource[] = [];
      const tasks: Task[] = [];
      const triggers: Trigger[] = [];

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
            node_did,
            schedule_id,
            display_name: cadenceInWords({ node_did, schedule_id, cadence }),
            cadence,
          });
        }
        source = { kind: "schedule", schedule_id };
      } else if (kind === "event") {
        let event_source_id = eventId;
        if (eventId === NEW) {
          const source_collection = watch === OTHER ? collection.trim() : watch;
          if (!source_collection) throw new Error("Choose what to watch.");
          event_source_id = idFor("event", "evsrc");
          const doc = {
            node_did,
            event_source_id,
            display_name: null as string | null,
            source_collection,
            event_kind: "created",
          };
          doc.display_name = eventInWords(doc);
          eventSources.push(doc);
        }
        source = { kind: "event", event_source_id };
      }

      let task_id = taskId;
      if (taskId === NEW) {
        task_id = idFor("task", "task");
        tasks.push({
          node_did,
          task_id,
          display_name: displayName,
          description: null,
          agent_id: agentId,
          prompt_template: prompt.trim(),
          emit_outcome: false,
          goal_objective_template: null,
          goal_token_budget: null,
          enabled: true,
          output_schema_ref: null,
        });
      }

      if (source)
        triggers.push({
          node_did,
          trigger_id,
          display_name: displayName,
          task_id,
          source,
          enabled: enable && !agentOff,
          concurrency: "serial",
        });
      await changeConfig("applyConfigComponents", {
        document: {
          node: { node_did },
          ...(schedules.length ? { schedules } : {}),
          ...(eventSources.length ? { event_sources: eventSources } : {}),
          ...(tasks.length ? { tasks } : {}),
          ...(triggers.length ? { triggers } : {}),
        },
      });
      if (source && onCreated) onCreated(trigger_id);
      else if (!source && onCreatedTask) onCreatedTask(task_id);
      else
        navigate(
          source
            ? {
                name: "agent",
                nodeDid: node_did,
                section: "triggers",
                item: trigger_id,
              }
            : { name: "agent", nodeDid: node_did, section: "tasks", item: task_id },
        );
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="grid gap-4">
      {!task && (
        <Input
          id="auto-name"
          aria-label="Name"
          autoFocus={!start}
          placeholder={fromPrompt || "Name it"}
          value={name}
          onChange={(e) => setName(e.target.value)}
          className="h-auto rounded-none border-0 bg-transparent px-0 py-0 font-heading text-lg font-medium text-heading shadow-none focus-visible:ring-0 md:text-lg dark:bg-transparent"
        />
      )}
      <section aria-labelledby="auto-when" className="grid gap-3">
        <h3 id="auto-when" className="text-sm">
          When should it run?
        </h3>
        <div
          role="radiogroup"
          aria-labelledby="auto-when"
          className="grid gap-2 sm:grid-cols-3"
        >
          {KINDS.filter((k) => k.id !== "manual" || !task).map((k) => (
            <button
              key={k.id}
              type="button"
              role="radio"
              aria-checked={kind === k.id}
              onClick={() => setKind(k.id)}
              className={cn(
                "flex cursor-pointer items-start gap-2.5 rounded-xl border px-3 py-2.5 text-left transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
                kind === k.id
                  ? "border-ring bg-raised"
                  : "border-border/60 hover:bg-accent/50",
              )}
            >
              <k.icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
              <span className="grid gap-0.5">
                <span className="text-sm">{k.label}</span>
                <span className="text-xs text-muted-foreground">{k.hint}</span>
              </span>
            </button>
          ))}
        </div>
        {kind === "schedule" && scheduleId === NEW && (
          <div
            role="radiogroup"
            aria-label="How often"
            className="flex flex-wrap gap-1.5"
          >
            {PRESETS.map((p) => (
              <button
                key={p.id}
                type="button"
                role="radio"
                aria-checked={preset === p.id}
                onClick={() => setPreset(p.id)}
                className={cn(
                  "cursor-pointer rounded-full border px-3 py-1 text-xs transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
                  preset === p.id
                    ? "border-ring bg-raised text-foreground"
                    : "border-border/60 text-muted-foreground hover:text-foreground",
                )}
              >
                {SHORT[p.id]}
              </button>
            ))}
          </div>
        )}
        {kind === "schedule" && scheduleId === NEW && preset === "custom" && (
          <Input
            aria-label="Cron expression"
            className="font-mono"
            placeholder="0 9 * * 1-5"
            value={cron}
            onChange={(e) => setCron(e.target.value)}
          />
        )}
        {kind === "event" && eventId === NEW && (
          <Field>
            {/* the runtime fires event triggers only for new documents */}
            <FieldLabel htmlFor="auto-collection">
              When something new is added to
            </FieldLabel>
            <CollectionPicker
              id="auto-collection"
              node={deployment}
              value={watch}
              onPick={setWatch}
            />
            {watch === OTHER && (
              <Input
                aria-label="Collection name"
                autoFocus
                className="font-mono"
                placeholder="MailboxItem"
                value={collection}
                onChange={(e) => setCollection(e.target.value)}
              />
            )}
          </Field>
        )}
      </section>
      {taskId === NEW ? (
        <Textarea
          aria-label="What the agent should do"
          autoFocus={Boolean(start)}
          placeholder="What should the agent do? For example: read the mailbox items from the last day and post a short digest."
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          className={cn(
            "overflow-y-auto",
            layout === "page" ? "min-h-40 max-h-[50vh]" : "min-h-28 max-h-64",
          )}
        />
      ) : (
        <p className="text-sm text-muted-foreground">
          Runs {existingTask?.name ?? taskId}, with its own prompt and agent.
        </p>
      )}
      {taskId === NEW && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          Runs with
          <AgentPicker
            deployment={deployment}
            agentId={agentId}
            onChange={setAgentId}
          />
        </div>
      )}
      {agentOff && agent && (
        <p className="text-sm text-warning">
          {agent.displayName} can’t run ({readiness?.reason}), so the automation is
          created off.
        </p>
      )}
      {canReuse &&
        (reuse ? (
          <div className="grid gap-3 rounded-xl bg-raised/50 p-3">
            {kind === "schedule" && (
              <Choice
                id="auto-schedule"
                label="Schedule"
                value={scheduleId}
                onChange={setScheduleId}
                items={deployment.schedules.map((s) => ({
                  value: s.schedule_id,
                  label: s.display_name ?? cadenceInWords(s),
                }))}
                newLabel="Its own schedule"
              />
            )}
            {kind === "event" && (
              <Choice
                id="auto-event"
                label="Event"
                value={eventId}
                onChange={setEventId}
                items={deployment.eventSources.map((e) => ({
                  value: e.event_source_id,
                  label: e.display_name ?? eventInWords(e),
                }))}
                newLabel="Its own event"
              />
            )}
            {!task && (
              <Choice
                id="auto-task"
                label="Task"
                value={taskId}
                onChange={setTaskId}
                items={deployment.tasks.map((t) => ({
                  value: t.taskId,
                  label: t.name ?? t.taskId,
                }))}
                newLabel="Its own task"
              />
            )}
          </div>
        ) : (
          <button
            type="button"
            onClick={() => setReuse(true)}
            className="w-fit cursor-pointer text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
          >
            Use an existing schedule or task
          </button>
        ))}
      {error && <FieldError>{error}</FieldError>}
      <div
        className={cn(
          "flex flex-wrap items-center gap-2",
          layout === "page" && "border-t border-border/60 pt-4",
        )}
      >
        {kind !== "manual" && (
          <span className="flex items-center gap-2 text-sm">
            <Checkbox
              id="auto-enable"
              checked={enable && !agentOff}
              disabled={agentOff}
              onCheckedChange={(v) => setEnable(Boolean(v))}
            />
            <label htmlFor="auto-enable">Turn on now</label>
          </span>
        )}
        <span className="flex-1" />
        <Button variant="ghost" disabled={busy} onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="brand" disabled={busy} onClick={() => void create()}>
          {busy ? "Creating…" : "Create"}
        </Button>
      </div>
    </div>
  );
}

/** The same form in a dialog, for the node configuration's own buttons. */
export function NewAutomationDialog({
  deployment,
  open,
  onOpenChange,
  forTask = false,
  task,
  initialKind,
  onCreated,
  template,
}: {
  deployment: NodeView;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  forTask?: boolean;
  task?: string;
  initialKind?: "schedule" | "event";
  onCreated?: (triggerId: string) => void;
  template?: AutomationStart | null;
}) {
  const [opened, setOpened] = useState(0);
  const change = (next: boolean) => {
    if (next) setOpened((n) => n + 1);
    onOpenChange(next);
  };
  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogContent aria-modal="true" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {forTask ? "New task" : task ? "When it runs" : "New automation"}
          </DialogTitle>
          <DialogDescription className="sr-only">
            What the agent should do, when it runs, and which agent runs it.
          </DialogDescription>
        </DialogHeader>
        {open && (
          <AutomationForm
            /* a fresh form each time it opens */
            key={opened}
            deployment={deployment}
            forTask={forTask}
            task={task}
            initialKind={initialKind}
            start={template}
            layout="dialog"
            onCancel={() => onOpenChange(false)}
            onCreated={(triggerId) => {
              onOpenChange(false);
              if (onCreated) onCreated(triggerId);
              else
                navigate({
                  name: "agent",
                  nodeDid: deployment.nodeDid,
                  section: "triggers",
                  item: triggerId,
                });
            }}
            onCreatedTask={(taskId) => {
              onOpenChange(false);
              navigate({
                name: "agent",
                nodeDid: deployment.nodeDid,
                section: "tasks",
                item: taskId,
              });
            }}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}
