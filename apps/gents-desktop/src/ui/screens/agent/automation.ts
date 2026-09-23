/* Automations in words. A trigger is a sentence, "Every weekday at 09:00 ·
   run Nightly audit with Implementer", and a readiness line that says why
   it will not fire. All from the 7.3 documents, nothing invented. */
import { behaviorReadiness } from "@/lib/behavior-readiness";
import type {
  DeploymentView,
  EventSource,
  Schedule,
  Tools,
  TriggerView,
} from "@source-inc/gents-desktop-client";

const DAYS = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
];
const hhmm = (h: string, m: string) => `${h.padStart(2, "0")}:${m.padStart(2, "0")}`;
const plural = (n: number, unit: string) => `${n} ${unit}${n === 1 ? "" : "s"}`;

/* the common cron shapes; anything else is shown as its expression */
export function cadenceInWords(s: Schedule): string {
  if (s.cadence.kind === "interval") {
    const secs = s.cadence.interval_secs;
    if (secs % 3600 === 0)
      return `Every ${secs === 3600 ? "hour" : plural(secs / 3600, "hour")}`;
    if (secs % 60 === 0)
      return `Every ${secs === 60 ? "minute" : plural(secs / 60, "minute")}`;
    return `Every ${plural(secs, "second")}`;
  }
  const parts = s.cadence.expression.trim().split(/\s+/);
  if (parts.length !== 5) return `Cron ${s.cadence.expression}`;
  const [min, hour, dom, mon, dow] = parts;
  const num = /^\d+$/;
  if (dom === "*" && mon === "*" && num.test(min) && num.test(hour)) {
    const at = hhmm(hour, min);
    if (dow === "*") return `Every day at ${at}`;
    if (dow === "1-5") return `Every weekday at ${at}`;
    if (dow === "0,6" || dow === "6,0") return `Every weekend day at ${at}`;
    if (num.test(dow) && Number(dow) <= 6) return `Every ${DAYS[Number(dow)]} at ${at}`;
  }
  if (hour === "*" && dom === "*" && mon === "*" && dow === "*") {
    if (min === "*") return "Every minute";
    const m = /^\*\/(\d+)$/.exec(min);
    if (m) return `Every ${plural(Number(m[1]), "minute")}`;
  }
  if (dom === "*" && mon === "*" && dow === "*" && num.test(min)) {
    const h = /^\*\/(\d+)$/.exec(hour);
    if (h) return `Every ${plural(Number(h[1]), "hour")}`;
    if (hour === "*") return `Every hour at :${min.padStart(2, "0")}`;
  }
  return `Cron ${s.cadence.expression}`;
}

export function eventInWords(e: EventSource): string {
  const kind = e.event_kind ?? "changed";
  const article = /^[aeiou]/i.test(e.source_collection) ? "an" : "a";
  return `When ${article} ${e.source_collection} is ${kind}${e.filter ? " matching a filter" : ""}`;
}

export function sourceInWords(
  deployment: DeploymentView,
  t: TriggerView,
): string | null {
  const src = t.config.source;
  if (src.kind === "schedule") {
    const s = deployment.schedules.find((x) => x.schedule_id === src.schedule_id);
    return s ? cadenceInWords(s) : null;
  }
  const e = deployment.eventSources.find(
    (x) => x.event_source_id === src.event_source_id,
  );
  return e ? eventInWords(e) : null;
}

export type Readiness =
  { ok: true; note: string | null } | { ok: false; reason: string };

/* why a trigger will not fire, in the order a user can fix it: its own
   switch and references from the loaded documents, then the behavior's
   readiness as the bridge reports it. `ok` means nothing here blocks it;
   it is never shown as a promise that the run will succeed. */
export function triggerReadiness(
  deployment: DeploymentView,
  t: TriggerView,
): Readiness {
  const cfg = t.config;
  if (cfg.enabled === false) return { ok: false, reason: "Off" };
  if (sourceInWords(deployment, t) === null)
    return {
      ok: false,
      reason:
        cfg.source.kind === "schedule"
          ? "Schedule is missing"
          : "Event source is missing",
    };
  const task = deployment.tasks.find((x) => x.taskId === cfg.task_id);
  if (!task) return { ok: false, reason: "Task is missing" };
  if (task.enabled === false) return { ok: false, reason: "Task is disabled" };
  const b = deployment.behaviors.find((x) => x.behaviorId === task.behaviorId);
  if (!b) return { ok: false, reason: "Behavior is missing" };
  /* whether the behavior can run is the bridge's readiness decision, not a
     guess from the documents here */
  const readiness = behaviorReadiness(deployment, b.behaviorId);
  if (!readiness.ready)
    return { ok: false, reason: `${b.displayName}: ${readiness.reason}` };
  if (t.lastStatus === "failed")
    return {
      ok: true,
      note: `Last run failed${t.lastError ? `: ${t.lastError}` : ""}`,
    };
  if (t.nextRunAt)
    return { ok: true, note: `Next ${new Date(t.nextRunAt).toLocaleString()}` };
  return { ok: true, note: null };
}

/* the sentence's second half: what runs, and with whom */
export function actionInWords(deployment: DeploymentView, t: TriggerView): string {
  const task = deployment.tasks.find((x) => x.taskId === t.config.task_id);
  if (!task) return "run a missing task";
  const b = deployment.behaviors.find((x) => x.behaviorId === task.behaviorId);
  return `run ${task.name ?? task.taskId}${b ? ` with ${b.displayName}` : ""}`;
}

/* a profile by name, with its model beside it so the field reads as a model picker */
export function profileLabel(deployment: DeploymentView, profileId: string) {
  const p = deployment.inferenceProfiles.find((x) => x.profile_id === profileId);
  if (!p) return profileId;
  const name = p.display_name ?? p.profile_id;
  return name.includes(p.model_name) ? name : `${name} · ${p.model_name}`;
}

/* what a Tools document lets a behavior do, in one line: "files rw · bash on · no network" */
export function toolsInWords(tools: Tools | null | undefined): string {
  if (!tools) return "no tools";
  const files = tools.host?.files?.mode;
  const bash = tools.host?.bash;
  const parts = [
    files === "ReadWrite" ? "files rw" : files === "ReadOnly" ? "files ro" : "no files",
    !bash || bash.mode === "Off"
      ? "no bash"
      : bash.mode === "Unrestricted"
        ? "any command"
        : "read-only commands",
  ];
  if (bash && bash.mode !== "Off")
    parts.push(bash.network_mode === "enabled" ? "network" : "no network");
  if (tools.remote) parts.push("remote");
  if (tools.subagents) parts.push("subagents");
  return parts.join(" · ");
}
