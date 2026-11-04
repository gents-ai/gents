import { useState } from "react";
import { ArrowUpRight, Play } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { href, navigate } from "@/lib/router";
import { toastFailure } from "@/lib/failure";
import { useApp } from "@/app/AppContext";
import type { NodeView } from "../../../hooks/fleetStore";
import { Hint } from "../Hint";
import { useMinute, when } from "../time";
import { AutomationMenu } from "./AutomationMenu";
import { MANUAL, runsOf, type Automation } from "./model";

/* a template that names {{placeholders}} needs values Run now can't give */
const placeholdersIn = (a: Automation) => [
  ...new Set(
    [
      ...`${a.task?.promptTemplate ?? ""}\n${a.task?.goalObjectiveTemplate ?? ""}`.matchAll(
        /\{\{\s*([\w.]+)\s*\}\}/g,
      ),
    ].map((m) => m[1]!),
  ),
];

/** One automation beside the list: what it does and when, whether it can
    run, Run now, and its recent runs with the sessions they started. The
    full settings stay in the node's configuration, a link away. */
export function AutomationDetail({
  node,
  automation: a,
}: {
  node: NodeView;
  automation: Automation;
}) {
  const now = useMinute();
  const {
    actions: { runTask },
  } = useApp();
  const [running, setRunning] = useState(false);
  const settings = {
    name: "agent" as const,
    nodeDid: node.nodeDid,
    section: a.trigger ? "triggers" : "tasks",
    item: a.trigger ? a.trigger.config.trigger_id : a.id.slice(MANUAL.length),
  };
  const missing = placeholdersIn(a);
  const blocked = !a.task
    ? "Its task is missing"
    : a.task.enabled === false
      ? "Turn its task on to run it"
      : missing.length
        ? `Needs ${missing.join(", ")}: run it from its task's settings`
        : null;
  const run = async () => {
    if (!a.task) return;
    setRunning(true);
    try {
      const started = await runTask({
        taskId: a.task.taskId,
        nodeDid: node.nodeDid,
        args: {},
      });
      toast(`${a.name} started`, {
        action: {
          label: "Open session",
          onClick: () => navigate({ name: "session", sessionId: started.sessionId }),
        },
      });
    } catch (error) {
      toastFailure("run the automation", error);
    } finally {
      setRunning(false);
    }
  };
  const runs = runsOf(a);
  const state = !a.readiness.ok
    ? a.readiness.reason
    : a.lastFailed
      ? `Last run failed${a.lastError ? `: ${a.lastError}` : ""}`
      : (a.readiness.note ?? (a.on ? "Ready" : "Off"));
  return (
    <article className="grid gap-6" data-testid="automation-detail">
      <header className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <h2 className="font-heading text-lg font-medium text-heading">{a.name}</h2>
          <p className="mt-0.5 text-sm text-muted-foreground">
            {a.when} · {a.does}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Hint label={blocked ?? "Start it now, as its schedule or event would"}>
            <span>
              <Button
                size="sm"
                variant="brand"
                disabled={blocked !== null || running}
                onClick={() => void run()}
              >
                <Play /> {running ? "Starting…" : "Run now"}
              </Button>
            </span>
          </Hint>
          <AutomationMenu node={node} automation={a} />
        </div>
      </header>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-1.5 text-sm">
        <dt className="text-muted-foreground">Status</dt>
        <dd
          className={cn(
            a.readiness.ok && !a.lastFailed ? "text-foreground" : "text-warning",
          )}
        >
          {state}
        </dd>
        <dt className="text-muted-foreground">Runs</dt>
        <dd className="tabular-nums">{a.runs}</dd>
        <dt className="text-muted-foreground">Last run</dt>
        <dd>{a.lastRunAt ? `${when(a.lastRunAt, now)} ago` : "Never"}</dd>
        {a.nextRunAt && (
          <>
            <dt className="text-muted-foreground">Next run</dt>
            <dd>{new Date(a.nextRunAt).toLocaleString()}</dd>
          </>
        )}
      </dl>
      <section aria-label="Recent runs" className="grid gap-1">
        <h3 className="font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
          Recent runs
        </h3>
        {runs.length === 0 ? (
          <p className="text-sm text-muted-foreground">No runs recorded yet.</p>
        ) : (
          <ul className="grid">
            {runs.map((r) => (
              <li
                key={r.requestId}
                className="flex items-center gap-3 border-b border-border/40 py-2 text-sm last:border-0"
              >
                <span className="w-16 shrink-0 text-muted-foreground tabular-nums">
                  {when(r.createdAt, now)} ago
                </span>
                <span className="min-w-0 flex-1 truncate">
                  {r.causedByTriggerKind
                    ? `By its ${r.causedByTriggerKind}`
                    : "By hand"}
                  {" · "}
                  {(r.lifecycleState ?? "unknown").replace(/_/g, " ")}
                </span>
                {r.sessionId && (
                  <a
                    href={href({ name: "session", sessionId: r.sessionId })}
                    className="shrink-0 text-xs underline-offset-2 hover:underline"
                  >
                    Open session
                  </a>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
      <a
        href={href(settings)}
        className="inline-flex w-fit items-center gap-1 text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
      >
        All its settings <ArrowUpRight className="size-3" />
      </a>
    </article>
  );
}
