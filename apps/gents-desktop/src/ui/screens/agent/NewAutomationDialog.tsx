import type { NodeView } from "../../../hooks/fleetStore";
import { agentReadiness } from "@/lib/agent-readiness";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, Hand, Timer, Zap } from "lucide-react";
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
import { Hint } from "../Hint";
import { CollectionPicker } from "./CollectionPicker";

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
type Concurrency = NonNullable<Trigger["concurrency"]>;

/* what happens when it's due while a run is still going */
const OVERLAP: { value: Concurrency; label: string }[] = [
  { value: "serial", label: "Skip that run" },
  { value: "queued_serial", label: "Run each one afterwards, in turn" },
  { value: "latest_only", label: "Run once afterwards, for the latest" },
  { value: "parallel", label: "Run them at the same time" },
];
type Kind = "schedule" | "event" | "manual";

/** A starting point: its name, schedule and prompt fill the form. */
export type AutomationStart = {
  name: string;
  preset: Preset;
  prompt: string;
};

function Choice({
  id,
  value,
  onChange,
  items,
  newLabel,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  items: { value: string; label: string }[];
  newLabel: string;
}) {
  const all = [...items, { value: NEW, label: newLabel }];
  return (
    <Select items={all} value={value} onValueChange={(v) => v && onChange(v)}>
      <SelectTrigger id={id} className="w-64 max-w-full">
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
  );
}

/* one of More options: its label on the left, its control on the right */
function OptionRow({
  label,
  htmlFor,
  hint,
  children,
}: {
  label: string;
  htmlFor: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    /* side by side where there's room; on a phone the control sits under
       its label and takes the row's width */
    <div className="grid gap-2 px-4 py-2.5 sm:flex sm:items-center sm:justify-between sm:gap-4">
      {hint ? (
        <Hint label={hint}>
          <label
            htmlFor={htmlFor}
            className="text-sm underline decoration-muted-foreground/50 decoration-dotted underline-offset-4"
          >
            {label}
          </label>
        </Hint>
      ) : (
        <label htmlFor={htmlFor} className="text-sm">
          {label}
        </label>
      )}
      <div className="min-w-0 max-sm:[&>*]:w-full">{children}</div>
    </div>
  );
}

/* a positive whole number typed in a field, or null when it's left empty */
function wholeNumber(text: string, what: string): number | null {
  if (!text.trim()) return null;
  const n = Number(text);
  if (!Number.isInteger(n) || n <= 0)
    throw new Error(`${what} is a whole number above 0.`);
  return n;
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
    defaultAgentOf(deployment)?.agentDid ?? deployment.agents[0]?.agentDid ?? "";
  const [name, setName] = useState(start?.name ?? "");
  const [kind, setKind] = useState<Kind | null>(
    start ? "schedule" : forTask ? "manual" : (initialKind ?? null),
  );
  const [scheduleId, setScheduleId] = useState(NEW);
  const [preset, setPreset] = useState<Preset | null>(start?.preset ?? null);
  const [cron, setCron] = useState("");
  const [eventId, setEventId] = useState(NEW);
  // The watched collection must be explicit to avoid self-triggering task writes.
  const [description, setDescription] = useState("");
  const [concurrency, setConcurrency] = useState<Concurrency>("serial");
  const [goal, setGoal] = useState("");
  const [goalBudget, setGoalBudget] = useState("");
  const [watch, setWatch] = useState("");
  const [filter, setFilter] = useState("");
  const [groupCount, setGroupCount] = useState("");
  const [groupField, setGroupField] = useState("");
  const [groupWait, setGroupWait] = useState("");
  const [taskId, setTaskId] = useState(task ?? NEW);
  const [enable, setEnable] = useState(false);
  const [more, setMore] = useState(false);
  const ids = useRef<Record<string, string>>({});
  const idFor = (key: string, prefix: string) => (ids.current[key] ??= newId(prefix));
  const [prompt, setPrompt] = useState(start?.prompt ?? "");
  const [agentDid, setAgentDid] = useState(defaultAgent);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const existingTask =
    taskId === NEW ? null : deployment.tasks.find((t) => t.taskId === taskId);
  const runningAgentDid = taskId === NEW ? agentDid : existingTask?.agentDid;
  const agent = agentOf(deployment, runningAgentDid) ?? null;
  const readiness = agent ? agentReadiness(deployment, agent.agentDid) : null;
  const agentOff = readiness !== null && !readiness.ready;
  const fromPrompt = prompt.trim().split("\n")[0].slice(0, 60);

  const hasMore = () => taskId === NEW || kind !== "manual" || canReuse;
  const canReuse =
    !forTask &&
    (deployment.schedules.length > 0 ||
      deployment.eventSources.length > 0 ||
      (!task && deployment.tasks.length > 0));

  // an answer to the last refusal clears it
  useEffect(
    () => setError(null),
    [
      prompt,
      kind,
      preset,
      watch,
      agentDid,
      taskId,
      goal,
      goalBudget,
      groupCount,
      groupField,
      groupWait,
    ],
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
    if (taskId === NEW && !agentDid) {
      setError("Choose the agent that runs it.");
      return;
    }
    const budget = goalBudget.trim() ? Number(goalBudget) : null;
    if (budget !== null && !(Number.isInteger(budget) && budget > 0)) {
      setError("A token budget is a whole number of tokens.");
      return;
    }
    if (budget !== null && !goal.trim()) {
      setError("A token budget needs a goal.");
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
          const source_collection = watch.trim();
          if (!source_collection) throw new Error("Choose what to watch.");
          const count = wholeNumber(groupCount, "How many to wait for");
          const wait = wholeNumber(groupWait, "How long to wait");
          if ((count !== null || wait !== null) && !groupField.trim())
            throw new Error("Say which field a group's documents share.");
          event_source_id = idFor("event", "evsrc");
          const doc: EventSource = {
            node_did,
            event_source_id,
            display_name: null,
            source_collection,
            event_kind: "created",
            filter: filter.trim() || null,
            correlation_field: groupField.trim() || null,
            group:
              count !== null || wait !== null
                ? { expected_count: count, timeout_secs: wait }
                : null,
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
          description: description.trim() || null,
          agent_did: agentDid,
          prompt_template: prompt.trim(),
          emit_outcome: false,
          goal_objective_template: goal.trim() || null,
          goal_token_budget: budget,
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
          concurrency,
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
      <section aria-labelledby="auto-when" className="grid gap-2.5">
        <h3 id="auto-when" className="text-sm">
          When should it run?
        </h3>
        <div
          role="radiogroup"
          aria-labelledby="auto-when"
          className="flex flex-wrap gap-1.5"
        >
          {KINDS.filter((k) => k.id !== "manual" || !task).map((k) => (
            <Hint key={k.id} label={k.hint}>
              <button
                type="button"
                role="radio"
                aria-checked={kind === k.id}
                onClick={() => setKind(k.id)}
                className={cn(
                  "inline-flex cursor-pointer items-center gap-2 rounded-full border px-3 py-1.5 text-sm transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
                  kind === k.id
                    ? "border-ring bg-raised text-foreground"
                    : "border-border/60 text-muted-foreground hover:text-foreground",
                )}
              >
                <k.icon className="size-4" />
                {k.label}
              </button>
            </Hint>
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
            layout === "page" ? "min-h-24 max-h-[50vh]" : "min-h-20 max-h-64",
          )}
        />
      ) : (
        <p className="text-sm text-muted-foreground">
          Runs {existingTask?.name ?? taskId}, with its own prompt and agent.
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
        {taskId === NEW && (
          <>
            Runs with
            <AgentPicker
              deployment={deployment}
              agentDid={agentDid}
              onChange={setAgentDid}
            />
          </>
        )}
        {hasMore() && (
          <button
            type="button"
            aria-expanded={more}
            onClick={() => setMore(!more)}
            className="ml-auto inline-flex cursor-pointer items-center gap-1 text-xs hover:text-foreground"
          >
            More options
            <ChevronRight
              aria-hidden
              className={cn("size-3.5 transition-transform", more && "rotate-90")}
            />
          </button>
        )}
      </div>
      {more && (
        <div className="divide-y divide-border/60 rounded-xl bg-raised/50">
          {kind !== "manual" && (
            <OptionRow label="If it's still running when due" htmlFor="auto-overlap">
              <Select
                items={OVERLAP}
                value={concurrency}
                onValueChange={(v) => v && setConcurrency(v as Concurrency)}
              >
                <SelectTrigger id="auto-overlap" className="w-64 max-w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {OVERLAP.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </OptionRow>
          )}
          {kind === "event" && eventId === NEW && (
            <>
              <OptionRow
                label="Only when"
                htmlFor="auto-filter"
                hint="A filter on the new document's fields, written as the database's filters are."
              >
                <Input
                  id="auto-filter"
                  className="w-64 max-w-full font-mono text-xs"
                  placeholder='{ status: { _eq: "open" } }'
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                />
              </OptionRow>
              <OptionRow
                label="Wait for a group"
                htmlFor="auto-group-count"
                hint="Run once for several new documents that share a field: when that many have arrived, or when the wait runs out."
              >
                <span className="flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
                  <Input
                    id="auto-group-count"
                    aria-label="How many to wait for"
                    inputMode="numeric"
                    className="w-16"
                    placeholder="3"
                    value={groupCount}
                    onChange={(e) => setGroupCount(e.target.value)}
                  />
                  sharing
                  <Input
                    aria-label="Field they share"
                    className="w-32 font-mono text-xs"
                    placeholder="upload_id"
                    value={groupField}
                    onChange={(e) => setGroupField(e.target.value)}
                  />
                  for up to
                  <Input
                    aria-label="How long to wait, in seconds"
                    inputMode="numeric"
                    className="w-16"
                    placeholder="60"
                    value={groupWait}
                    onChange={(e) => setGroupWait(e.target.value)}
                  />
                  s
                </span>
              </OptionRow>
            </>
          )}
          {taskId === NEW && (
            <>
              <OptionRow
                label="Keep going until"
                htmlFor="auto-goal"
                hint="A goal it works toward across turns, instead of stopping after one reply."
              >
                <Input
                  id="auto-goal"
                  className="w-64 max-w-full"
                  placeholder="Every failing test passes"
                  value={goal}
                  onChange={(e) => setGoal(e.target.value)}
                />
              </OptionRow>
              <OptionRow label="Token budget" htmlFor="auto-goal-budget">
                <Input
                  id="auto-goal-budget"
                  inputMode="numeric"
                  className="w-32"
                  placeholder="No limit"
                  value={goalBudget}
                  onChange={(e) => setGoalBudget(e.target.value)}
                />
              </OptionRow>
              <OptionRow label="Description" htmlFor="auto-description">
                <Input
                  id="auto-description"
                  className="w-64 max-w-full"
                  placeholder="What it's for, in a line"
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                />
              </OptionRow>
            </>
          )}
          {canReuse && kind === "schedule" && (
            <OptionRow label="Schedule" htmlFor="auto-schedule">
              <Choice
                id="auto-schedule"
                value={scheduleId}
                onChange={setScheduleId}
                items={deployment.schedules.map((s) => ({
                  value: s.schedule_id,
                  label: s.display_name ?? cadenceInWords(s),
                }))}
                newLabel="Its own schedule"
              />
            </OptionRow>
          )}
          {canReuse && kind === "event" && (
            <OptionRow label="Event" htmlFor="auto-event">
              <Choice
                id="auto-event"
                value={eventId}
                onChange={setEventId}
                items={deployment.eventSources.map((e) => ({
                  value: e.event_source_id,
                  label: e.display_name ?? eventInWords(e),
                }))}
                newLabel="Its own event"
              />
            </OptionRow>
          )}
          {canReuse && !task && (
            <OptionRow label="Task" htmlFor="auto-task">
              <Choice
                id="auto-task"
                value={taskId}
                onChange={setTaskId}
                items={deployment.tasks.map((t) => ({
                  value: t.taskId,
                  label: t.name ?? t.taskId,
                }))}
                newLabel="Its own task"
              />
            </OptionRow>
          )}
        </div>
      )}
      {agentOff && agent && (
        <p className="text-sm text-warning">
          {agent.displayName} can’t run ({readiness?.reason}), so the automation is
          created off.
        </p>
      )}
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
