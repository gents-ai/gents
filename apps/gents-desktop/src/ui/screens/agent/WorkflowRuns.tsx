/* A node's workflow runs as the Workflows page lists them: those going now,
   and each workflow's history. */

import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { JobState } from "@source-inc/gents-desktop-client/generated/JobState";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { useJobs } from "../../../hooks/jobs";
import { duration } from "@/lib/duration";
import { href } from "@/lib/router";
import { JobStatePill } from "../jobParts";
import { runAt } from "../WorkflowRunSheet";
import { span, useMinute } from "../time";
import { EmptyList } from "./ListDetail";
import { Group } from "./rows";

export { useJobs };
export const ACTIVE_STATES: JobState[] = ["running", "paused", "awaiting_user"];
export const ALL_STATES: JobState[] = [
  ...ACTIVE_STATES,
  "completed",
  "failed",
  "cancelled",
  "timed_out",
];

/** What the run view re-reads on: the run as the latest list shows it. */
export const observedOf = (run: JobView | undefined) =>
  run ? [run.state, run.progress?.revision, run.completed_at].join("|") : "gone";

export const runName = (run: JobView) => run.progress?.title || run.handle;

/** Every run on the node that is running, paused or waiting on the person,
    across sessions, as `gents automation job list` lists them. With none, it
    says so only as `empty`, and draws nothing without it. */
export function RunningRuns({
  runs,
  error,
  sessions,
  empty,
  onOpen,
}: {
  runs: JobView[] | null;
  error: string | null;
  sessions: readonly SessionSummary[];
  empty: string | null;
  onOpen: (run: JobView) => void;
}) {
  const now = useMinute();
  if (!error && runs?.length === 0)
    return empty ? (
      <div data-testid="running-empty">
        <EmptyList>{empty}</EmptyList>
      </div>
    ) : null;
  if (!error && !runs) return null;
  return (
    <Group title="Running">
      {error && <p className="px-5 py-3 text-sm text-destructive">{error}</p>}
      {runs?.map((run) => {
        const phase = run.progress?.phases.find((p) => p.state === "active")?.title;
        const budget = run.progress?.budget;
        const session = sessions.find((s) => s.sessionId === run.session_id);
        return (
          <div
            key={run.row_doc_id}
            data-testid="running-run"
            className="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-3 text-sm"
          >
            <button
              type="button"
              className="min-w-0 flex-1 truncate text-left font-medium hover:underline"
              onClick={() => onOpen(run)}
            >
              {runName(run)}
            </button>
            <JobStatePill state={run.state} />
            <span className="text-xs text-muted-foreground">
              {[
                phase,
                budget && `${budget.spent} of ${budget.total} agents`,
                `started ${span(now - run.created_at)} ago`,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
            <a
              href={href({
                name: "session",
                sessionId: run.session_id,
                nodeDid: run.scope.node_did,
              })}
              className="max-w-48 truncate text-xs underline decoration-border underline-offset-4 hover:decoration-foreground"
            >
              {session?.title ?? "Untitled session"}
            </a>
          </div>
        );
      })}
    </Group>
  );
}

/** One workflow's past runs, newest first: an installed plugin's by its
    `pack/name`, or a one-off script's by its id. */
export function RunHistory({
  nodeDid,
  plugin,
  scriptId,
  observed,
  onOpen,
}: {
  nodeDid: string;
  plugin?: string;
  scriptId?: string;
  observed: unknown;
  onOpen: (run: JobView) => void;
}) {
  const { runs, error } = useJobs(
    { nodeDid, states: ALL_STATES, ...(plugin ? { plugin } : {}) },
    observed,
  );
  const shown = scriptId
    ? runs?.filter(
        (run) => run.pin.origin === "dynamic" && run.pin.script_id === scriptId,
      )
    : runs;
  return (
    <div className="grid gap-1 px-5 pb-4 text-sm">
      {error && <p className="text-destructive">{error}</p>}
      {!shown && !error && <p className="text-muted-foreground">Loading runs…</p>}
      {shown?.length === 0 && (
        <p data-testid="history-empty" className="text-muted-foreground">
          No runs yet.
        </p>
      )}
      {!!shown?.length && (
        <ul data-testid="run-history" className="grid gap-1">
          {shown.map((run) => (
            <li key={run.row_doc_id}>
              <button
                type="button"
                data-testid="history-run"
                className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 rounded-md bg-surface px-2 py-1.5 text-left hover:bg-accent"
                onClick={() => onOpen(run)}
              >
                <JobStatePill state={run.state} />
                <span className="text-xs">{runAt(run.created_at)}</span>
                {run.completed_at != null && (
                  <span className="text-xs text-muted-foreground tabular-nums">
                    {duration(run.completed_at - run.created_at)}
                  </span>
                )}
                {run.outcome && (
                  <span className="font-mono text-xs text-destructive">
                    {run.outcome.code}
                  </span>
                )}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
