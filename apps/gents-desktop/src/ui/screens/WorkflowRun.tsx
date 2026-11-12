/* One workflow run as the node's operator reads it (`desktop_job_view`, the
   reader `gents automation job show` prints), wherever it is shown: the run
   sheet and the session panel's Workflows tab. The state is the view's own,
   derived from the row's lifecycle, never the progress record's. It re-reads
   when the caller's observation of the row changes; nothing here polls. */

import { useEffect, useState } from "react";
import { History, Pause, Square } from "lucide-react";
import type { JobControlKind } from "@source-inc/gents-desktop-client/generated/JobControlKind";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { duration } from "@/lib/duration";
import { errorText } from "../../lib/errorText";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { useHomeDid } from "../hooks/useClient";
import { isLocalNode } from "../lib/firstRun";
import { toastFailure } from "../lib/failure";
import { Hint } from "./Hint";
import {
  JobAgents,
  JobBudget,
  JobStages,
  JobStatePill,
  useAnswerJob,
} from "./jobParts";
import { PromoteWorkflowDialog } from "./PromoteWorkflowDialog";

export const runAt = (ms: number) =>
  new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });

/** An installed plugin by its `pack/name`; a one-off script by its id. */
export const whatRan = (view: JobView) =>
  view.pin.origin === "dynamic"
    ? `Script ${view.pin.script_id.slice(0, 8)}`
    : view.pin.plugin_ref;

type Read = { key: string; view: JobView | null; error: string | null };

/** A run read for as long as `active`, with the earlier runs opened along
    its resume chain. */
export function useWorkflowRun({
  nodeDid,
  rowDocId,
  observed,
  active,
}: {
  nodeDid: string;
  rowDocId: string;
  /* changes whenever the session observes a change to the row */
  observed: string;
  active: boolean;
}) {
  const { actions } = useApp();
  /* the earlier runs opened along the resume chain, latest last */
  const [trail, setTrail] = useState<string[]>([]);
  const [read, setRead] = useState<Read | null>(null);
  const shown = trail[trail.length - 1] ?? rowDocId;
  const key = `${nodeDid}\u0000${shown}`;
  useEffect(() => {
    if (!active) return;
    let current = true;
    actions.getJobView({ nodeDid, rowDocId: shown }).then(
      (view) => current && setRead({ key, view, error: null }),
      (e: unknown) =>
        current &&
        setRead((held) => ({
          key,
          view: held?.key === key ? held.view : null,
          error: errorText(e),
        })),
    );
    return () => {
      current = false;
    };
  }, [actions, active, nodeDid, shown, key, observed]);
  const current = read?.key === key ? read : null;
  return {
    nodeDid,
    view: current?.view ?? null,
    error: current?.error ?? null,
    loading: !current,
    trail,
    openEarlier: (rowDoc: string) => setTrail((held) => [...held, rowDoc]),
    back: () => setTrail((held) => held.slice(0, -1)),
    reset: () => setTrail([]),
  };
}

export type WorkflowRunRead = ReturnType<typeof useWorkflowRun>;

export const runTitle = (view: JobView | null) =>
  view ? view.progress?.title || view.handle : "Workflow run";

/** What a run is: its state and what ran, its stages, who is working, a
    question waiting on the person, how it ended, and its agents.
    `onLeave` is called when the person goes elsewhere from it (a child's
    session, or Answer), with whether the composer should take focus. */
export function WorkflowRunView({
  run,
  onLeave,
}: {
  run: WorkflowRunRead;
  onLeave: (toComposer: boolean) => void;
}) {
  const { actions } = useApp();
  const answerJob = useAnswerJob();
  const homeDid = useHomeDid();
  const runNode = useFleet((fleet) => nodeOf(fleet, run.nodeDid));
  const nodeIsLocal = !!runNode && isLocalNode(runNode, homeDid);
  const [promoting, setPromoting] = useState(false);
  const { view } = run;
  const progress = view?.progress ?? null;
  const control = (action: JobControlKind) => {
    if (!view) return;
    actions
      .controlJob({ nodeDid: run.nodeDid, toolCallDocId: view.row_doc_id, action })
      .catch((e: unknown) => toastFailure(action, e));
  };
  const answer = () => {
    if (!view) return;
    answerJob(view.handle, {
      sessionId: view.session_id,
      nodeDid: view.scope.node_did,
    });
    onLeave(true);
  };
  const promotable =
    !!view &&
    view.state === "completed" &&
    view.pin.origin === "dynamic" &&
    nodeIsLocal;

  return (
    <div className="grid gap-5 text-sm" data-testid="run-view">
      {run.trail.length > 0 && (
        <Button
          size="sm"
          variant="ghost"
          className="justify-self-start"
          onClick={run.back}
        >
          Back
        </Button>
      )}
      {run.error && (
        <p data-testid="run-error" className="text-destructive">
          {run.error}
        </p>
      )}
      {run.loading && <p className="text-muted-foreground">Loading run…</p>}
      {view && (
        <>
          <div className="grid gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <JobStatePill state={view.state} />
              <span
                data-testid="run-what"
                className="font-mono text-xs text-muted-foreground"
              >
                {whatRan(view)}
              </span>
              {view.state === "running" && (
                <span className="ml-auto flex gap-0.5">
                  <Hint label="Pause">
                    <Button
                      size="icon-xs"
                      variant="quiet"
                      aria-label="Pause"
                      onClick={() => control("pause")}
                    >
                      <Pause />
                    </Button>
                  </Hint>
                  <Hint label="Stop">
                    <Button
                      size="icon-xs"
                      variant="quiet"
                      aria-label="Stop"
                      onClick={() => control("stop")}
                    >
                      <Square />
                    </Button>
                  </Hint>
                </span>
              )}
            </div>
            <dl
              data-testid="run-times"
              className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-0.5 text-xs"
            >
              <dt className="text-muted-foreground">Started</dt>
              <dd>{runAt(view.created_at)}</dd>
              {view.completed_at != null && (
                <>
                  <dt className="text-muted-foreground">Finished</dt>
                  <dd>{runAt(view.completed_at)}</dd>
                  <dt className="text-muted-foreground">Duration</dt>
                  <dd data-testid="run-duration">
                    {duration(view.completed_at - view.created_at)}
                  </dd>
                </>
              )}
            </dl>
            {view.pin.resume_of && (
              <button
                type="button"
                data-testid="run-resumed-from"
                className="inline-flex items-center gap-1 justify-self-start text-xs text-muted-foreground underline decoration-border underline-offset-4 hover:text-foreground hover:decoration-foreground"
                onClick={() => run.openEarlier(view.pin.resume_of!)}
              >
                <History className="size-3" />
                Resumed from an earlier run
              </button>
            )}
          </div>
          {progress && (
            <section className="grid gap-3">
              <JobStages progress={progress} onOpenSession={() => onLeave(false)} />
              <JobBudget progress={progress} />
            </section>
          )}
          {view.state === "awaiting_user" && progress?.question && (
            <div className="flex items-center gap-2">
              <p data-testid="run-question" className="min-w-0 flex-1">
                {progress.question}
              </p>
              <Button size="sm" variant="outline" onClick={answer}>
                Answer
              </Button>
            </div>
          )}
          {(view.state === "failed" || view.state === "timed_out") && view.outcome && (
            <p data-testid="run-outcome">
              <span className="font-mono">{view.outcome.code}</span>{" "}
              {view.outcome.message}
            </p>
          )}
          {view.children.length > 0 && (
            <section className="grid gap-2">
              <h3 className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
                Agents
              </h3>
              <JobAgents agents={view.children} onOpenSession={() => onLeave(false)} />
            </section>
          )}
          {promotable && (
            <Button
              size="sm"
              variant="outline"
              className="justify-self-start"
              onClick={() => setPromoting(true)}
            >
              Promote
            </Button>
          )}
          {promoting && promotable && (
            <PromoteWorkflowDialog
              key={view.address}
              selector={view.address}
              onClose={() => setPromoting(false)}
            />
          )}
        </>
      )}
    </div>
  );
}
