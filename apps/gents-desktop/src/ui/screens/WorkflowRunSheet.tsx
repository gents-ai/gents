/* One workflow run as the node's operator reads it (`desktop_job_view`, the
   reader `gents automation job show` prints). The state is the view's own,
   derived from the row's lifecycle, never the progress record's. It re-reads
   when the cell's observation of the row changes; nothing here polls. */

import { useEffect, useRef, useState } from "react";
import type { JobControlKind } from "@source-inc/gents-desktop-client/generated/JobControlKind";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { duration } from "@/lib/duration";
import { href } from "@/lib/router";
import { compactTokens } from "@/lib/tokens";
import { errorText } from "../../lib/errorText";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { useHomeDid } from "../hooks/useClient";
import { isLocalNode } from "../lib/firstRun";
import { toastFailure } from "../lib/failure";
import { EditorSheet } from "./agent/EditorSheet";
import { JobProgressBody, JobStatePill, useAnswerJob, words } from "./jobParts";
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

export function WorkflowRunSheet({
  open,
  nodeDid,
  rowDocId,
  observed,
  onClose,
}: {
  open: boolean;
  nodeDid: string;
  rowDocId: string;
  /* changes whenever the session observes a change to the row */
  observed: string;
  onClose: () => void;
}) {
  const { actions } = useApp();
  const answerJob = useAnswerJob();
  const homeDid = useHomeDid();
  const runNode = useFleet((fleet) => nodeOf(fleet, nodeDid));
  const nodeIsLocal = !!runNode && isLocalNode(runNode, homeDid);
  /* the earlier runs opened along the resume chain, latest last */
  const [trail, setTrail] = useState<string[]>([]);
  const [promoting, setPromoting] = useState(false);
  const [read, setRead] = useState<Read | null>(null);
  const answering = useRef(false);
  const shown = trail[trail.length - 1] ?? rowDocId;
  const key = `${nodeDid}\u0000${shown}`;

  useEffect(() => {
    if (!open) return;
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
  }, [actions, open, nodeDid, shown, key, observed]);

  const close = () => {
    setTrail([]);
    setPromoting(false);
    onClose();
  };
  const current = read?.key === key ? read : null;
  const view = current?.view ?? null;
  const progress = view?.progress ?? null;
  const control = (action: JobControlKind) => {
    if (!view) return;
    actions
      .controlJob({ nodeDid, toolCallDocId: view.row_doc_id, action })
      .catch((e: unknown) => toastFailure(action, e));
  };
  const answer = () => {
    if (!view) return;
    answering.current = true;
    answerJob(view.handle, {
      sessionId: view.session_id,
      nodeDid: view.scope.node_did,
    });
    close();
  };
  const children = view
    ? [...view.children].sort(
        (a, b) => Number(b.state === "live") - Number(a.state === "live"),
      )
    : [];
  const promotable =
    !!view &&
    view.state === "completed" &&
    view.pin.origin === "dynamic" &&
    nodeIsLocal;

  return (
    <EditorSheet
      open={open}
      onClose={close}
      title={view ? progress?.title || view.handle : "Workflow run"}
      finalFocus={() => {
        if (!answering.current) return true;
        answering.current = false;
        return (
          document.querySelector<HTMLTextAreaElement>(
            '[data-testid="composer"] textarea',
          ) ?? true
        );
      }}
    >
      <div className="grid gap-5 text-sm" data-testid="run-view">
        {trail.length > 0 && (
          <Button
            size="sm"
            variant="ghost"
            className="justify-self-start"
            onClick={() => setTrail(trail.slice(0, -1))}
          >
            Back
          </Button>
        )}
        {current?.error && (
          <p data-testid="run-error" className="text-destructive">
            {current.error}
          </p>
        )}
        {!current && <p className="text-muted-foreground">Loading run…</p>}
        {view && (
          <>
            <div className="grid gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <JobStatePill state={view.state} />
                <span data-testid="run-what" className="font-mono text-xs">
                  {whatRan(view)}
                </span>
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
                <Button
                  size="sm"
                  variant="ghost"
                  className="justify-self-start"
                  data-testid="run-resumed-from"
                  onClick={() => setTrail([...trail, view.pin.resume_of!])}
                >
                  Resumed from an earlier run
                </Button>
              )}
            </div>
            {progress && (
              <div className="grid gap-2">
                <JobProgressBody state={view.state} progress={progress} />
              </div>
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
            {(view.state === "failed" || view.state === "timed_out") &&
              view.outcome && (
                <p data-testid="run-outcome">
                  <span className="font-mono">{view.outcome.code}</span>{" "}
                  {view.outcome.message}
                </p>
              )}
            {children.length > 0 && (
              <section className="grid gap-2">
                <h3 className="text-xs font-medium text-muted-foreground">Agents</h3>
                <ul data-testid="run-children" className="grid gap-1">
                  {children.map((child) => {
                    const name = child.title || child.slot;
                    return (
                      <li
                        key={child.slot}
                        data-testid="run-child"
                        data-state={child.state}
                        className="flex items-center gap-3 rounded-md bg-surface px-2 py-1.5"
                      >
                        <span className="min-w-0 flex-1 truncate">
                          {child.session_id ? (
                            <a
                              href={href({
                                name: "session",
                                sessionId: child.session_id,
                              })}
                              className="underline decoration-border underline-offset-4 hover:decoration-foreground"
                              onClick={close}
                            >
                              {name}
                            </a>
                          ) : (
                            name
                          )}
                        </span>
                        <span className="text-xs">{words(child.state)}</span>
                        <span className="text-xs text-muted-foreground tabular-nums">
                          {compactTokens(child.usage.total)} tokens
                        </span>
                        <span className="text-xs text-muted-foreground tabular-nums">
                          {duration(child.duration_ms)}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              </section>
            )}
            {(view.state === "running" || promotable) && (
              <div className="flex gap-1">
                {view.state === "running" && (
                  <>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => control("pause")}
                    >
                      Pause
                    </Button>
                    <Button size="sm" variant="outline" onClick={() => control("stop")}>
                      Stop
                    </Button>
                  </>
                )}
                {promotable && (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setPromoting(true)}
                  >
                    Promote
                  </Button>
                )}
              </div>
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
    </EditorSheet>
  );
}
