import type {
  TaskRunSummaryView,
  TaskView,
  TriggerView,
} from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../../hooks/fleetStore";
import { agentReadiness } from "@/lib/agent-readiness";
import {
  actionInWords,
  lastRunFailed,
  sourceInWords,
  triggerReadiness,
  type Readiness,
} from "../agent/automation";
import { packOrigin } from "../agent/ListDetail";

/** One thing that runs: a trigger and the task it starts, or a task with no
    trigger, which runs only by hand. */
export type Automation = {
  /** a trigger's id, or a hand-run task's id behind "task:" */
  id: string;
  kind: "schedule" | "event" | "manual";
  name: string;
  /** what it does, as "run X with Agent" */
  does: string;
  /** when it runs: the schedule or event in words, or "Run by hand" */
  when: string;
  readiness: Readiness;
  on: boolean;
  nextRunAt: string | null;
  lastRunAt: string | null;
  lastFailed: boolean;
  lastError: string | null;
  runs: number;
  task: TaskView | null;
  trigger: TriggerView | null;
  /** the pack it came from, as the configuration lists group it */
  pack: string | null;
};

export const MANUAL = "task:";
/** the page's item for a new automation; no automation id has this form */
export const NEW_AUTOMATION = "new";

const failedStatus = (status: string | null) =>
  status === "error" || status === "failed";

function fromTrigger(node: NodeView, t: TriggerView): Automation {
  const task = node.tasks.find((x) => x.taskId === t.config.task_id) ?? null;
  return {
    id: t.config.trigger_id,
    kind: t.config.source.kind,
    name: t.config.display_name ?? task?.name ?? t.config.trigger_id,
    does: actionInWords(node, t),
    when:
      sourceInWords(node, t) ??
      (t.config.source.kind === "schedule"
        ? "Schedule is missing"
        : "Event is missing"),
    readiness: triggerReadiness(node, t),
    on: t.config.enabled !== false && task?.enabled !== false,
    nextRunAt: t.nextRunAt,
    lastRunAt: t.lastAttemptAt,
    lastFailed: lastRunFailed(t),
    lastError: t.lastError,
    runs: t.fireCount ?? 0,
    task,
    trigger: t,
    pack: packOrigin(t.config.tags) ?? packOrigin(task?.tags),
  };
}

function fromTask(node: NodeView, task: TaskView): Automation {
  const agent = agentReadiness(node, task.agentId);
  const agentName =
    node.agents.find((a) => a.agentId === task.agentId)?.displayName ?? task.agentId;
  const readiness: Readiness =
    task.enabled === false
      ? { ok: false, reason: "Off" }
      : agent.ready
        ? { ok: true, note: null }
        : { ok: false, reason: `${agentName}: ${agent.reason}` };
  return {
    id: `${MANUAL}${task.taskId}`,
    kind: "manual",
    name: task.name ?? task.taskId,
    /* its name already says what it runs */
    does: `with ${agentName}`,
    when: "Run by hand",
    readiness,
    on: task.enabled !== false,
    nextRunAt: null,
    lastRunAt: task.recentRuns.lastAttemptAt,
    lastFailed: failedStatus(task.recentRuns.lastStatus),
    lastError: task.recentRuns.lastError,
    runs: task.recentRuns.totalFires,
    task,
    trigger: null,
    pack: packOrigin(task.tags),
  };
}

/** The node's automations: each trigger, then each task no trigger starts. */
export function automationsOf(node: NodeView): Automation[] {
  const triggered = new Set(node.triggers.map((t) => t.config.task_id));
  return [
    ...node.triggers.map((t) => fromTrigger(node, t)),
    ...node.tasks.filter((t) => !triggered.has(t.taskId)).map((t) => fromTask(node, t)),
  ];
}

/** Your own automations, then one group per pack by name; in each, the
    ones that run on their own before the ones run by hand. */
export function byPack(
  list: Automation[],
): { pack: string | null; items: Automation[] }[] {
  const order = (items: Automation[]) => [
    ...items.filter((a) => a.kind !== "manual"),
    ...items.filter((a) => a.kind === "manual"),
  ];
  const packs = [
    ...new Set(list.map((a) => a.pack).filter((p): p is string => p !== null)),
  ].sort();
  return [
    { pack: null, items: order(list.filter((a) => a.pack === null)) },
    ...packs.map((pack) => ({
      pack,
      items: order(list.filter((a) => a.pack === pack)),
    })),
  ].filter((g) => g.items.length > 0);
}

export type AutomationTab = "all" | "next" | "attention" | "off";

/** whether it needs a look: it can't run, or its last run failed */
export const needsAttention = (a: Automation) =>
  a.on && (!a.readiness.ok || a.lastFailed);

export function inTab(a: Automation, tab: AutomationTab, now: number): boolean {
  switch (tab) {
    case "all":
      return true;
    case "next":
      return a.on && a.nextRunAt !== null && Date.parse(a.nextRunAt) > now;
    case "attention":
      return needsAttention(a);
    case "off":
      return !a.on;
  }
}

/** a tab's automations in the order it reads them: Next up by when they
    run, the rest by name */
export function tabOrder(
  list: readonly Automation[],
  tab: AutomationTab,
): Automation[] {
  const sorted = [...list];
  if (tab === "next")
    sorted.sort((a, b) => Date.parse(a.nextRunAt!) - Date.parse(b.nextRunAt!));
  else sorted.sort((a, b) => a.name.localeCompare(b.name));
  return sorted;
}

export const matches = (a: Automation, query: string) => {
  const q = query.trim().toLowerCase();
  return !q || [a.name, a.does, a.when].some((s) => s.toLowerCase().includes(q));
};

/** an automation's recent runs, newest first: a trigger's own, or every run
    of a hand-run task */
export function runsOf(a: Automation, limit = 8): TaskRunSummaryView[] {
  const runs = a.task?.runHistory ?? [];
  const own = a.trigger
    ? runs.filter((r) => r.causedByTriggerId === a.trigger!.config.trigger_id)
    : runs;
  return [...own]
    .sort((x, y) => Date.parse(y.createdAt ?? "") - Date.parse(x.createdAt ?? ""))
    .slice(0, limit);
}
