/* A node's workflow runs, read with `listJobs` over the node's operator
   access, so a remote node reads as a local one does. The reader filters
   every job row on the node in memory and has no limit. */

import { useEffect, useState } from "react";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { JobState } from "@source-inc/gents-desktop-client/generated/JobState";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { useApp } from "@/app/AppContext";
import { href } from "@/lib/router";
import { errorText } from "../../../lib/errorText";
import { JobStatePill } from "../jobParts";
import { span, useMinute } from "../time";
import { Group } from "./rows";

export const ACTIVE_STATES: JobState[] = ["running", "paused", "awaiting_user"];

type JobsRequest = Parameters<DesktopApiAdapter["listJobs"]>[0];

/** The runs `request` names, read again whenever `observed` changes; a
    failed re-read keeps the runs it had. */
export function useJobs(request: JobsRequest | null, observed: unknown) {
  const { actions } = useApp();
  const key = request ? JSON.stringify(request) : null;
  const [read, setRead] = useState<{
    key: string;
    runs: JobView[] | null;
    error: string | null;
  } | null>(null);
  useEffect(() => {
    if (!key) return;
    let current = true;
    actions.listJobs(JSON.parse(key) as JobsRequest).then(
      (runs) => current && setRead({ key, runs, error: null }),
      (e: unknown) =>
        current &&
        setRead((held) => ({
          key,
          runs: held?.key === key ? held.runs : null,
          error: errorText(e),
        })),
    );
    return () => {
      current = false;
    };
  }, [actions, key, observed]);
  const current = read && read.key === key ? read : null;
  return { runs: current?.runs ?? null, error: current?.error ?? null };
}

/** What the run view re-reads on: the run as the latest list shows it. */
export const observedOf = (run: JobView | undefined) =>
  run ? [run.state, run.progress?.revision, run.completed_at].join("|") : "gone";

export const runName = (run: JobView) => run.progress?.title || run.handle;

/** Every run on the node that is running, paused or waiting on the person,
    across sessions, as `gents automation job list` lists them. */
export function RunningRuns({
  runs,
  error,
  sessions,
  onOpen,
}: {
  runs: JobView[] | null;
  error: string | null;
  sessions: readonly SessionSummary[];
  onOpen: (run: JobView) => void;
}) {
  const now = useMinute();
  return (
    <Group title="Running">
      {error && <p className="text-sm text-destructive">{error}</p>}
      {runs?.length === 0 && (
        <p data-testid="running-empty" className="py-2 text-sm text-muted-foreground">
          Nothing running.
        </p>
      )}
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
