/* A background job's row: where it stands, read from the fields the bridge
   carries for any job plugin. The state is the row's own (`jobState`), never
   the progress record's, which stays "running" on an ended row. The next
   calls are the bridge's, printed as given. */

import { useEffect, useState } from "react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import type { JobControlKind } from "@source-inc/gents-desktop-client/generated/JobControlKind";
import { Button } from "@gents/ui/components/button";
import { toastFailure } from "../lib/failure";
import { useApp } from "../app/AppContext";
import { PromoteWorkflowDialog } from "./PromoteWorkflowDialog";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { useHomeDid } from "../hooks/useClient";
import { isLocalNode } from "../lib/firstRun";
import { WorkflowRunSheet } from "./WorkflowRunSheet";
import {
  isTerminalJob,
  JobProgressBody,
  JobStatePill,
  useAnswerJob,
  words,
} from "./jobParts";

export function JobProgressCell({ tool }: { tool: RenderedToolCallView }) {
  const { actions, stores } = useApp();
  const answerJob = useAnswerJob();
  const sessionId = stores.selection.use.sessionId();
  const sessionNodeDid = stores.selection.use.nodeDid();
  const homeDid = useHomeDid();
  const sessionNode = useFleet((fleet) => nodeOf(fleet, sessionNodeDid));
  const sessionIsLocal = !!sessionNode && isLocalNode(sessionNode, homeDid);
  const [promotingKey, setPromotingKey] = useState<string | null>(null);
  const [runOpen, setRunOpen] = useState(false);
  const [promotion, setPromotion] = useState<{ key: string; names: string[] }>({
    key: "",
    names: [],
  });
  const state = tool.jobState;
  const progress = tool.jobProgress;
  const terminal = isTerminalJob(state);
  // Only a completed run is promotable; the host refuses the rest
  // (`promote_not_completed`).
  const promotable =
    state === "completed" &&
    tool.jobOrigin === "dynamic" &&
    sessionIsLocal &&
    tool.nodeDid === sessionNodeDid &&
    !!sessionId &&
    !!tool.toolCallId;
  const selector = `${sessionId}:${tool.toolCallId}`;
  const promotionKey = `${sessionNodeDid ?? ""}\u0000${selector}`;
  const promotedAs = promotion.key === promotionKey ? promotion.names : [];
  useEffect(() => {
    if (!promotable) return;
    let current = true;
    actions.getDynamicWorkflow(selector).then(
      (run) => current && setPromotion({ key: promotionKey, names: run.promoted_as }),
      () => {},
    );
    return () => {
      current = false;
    };
  }, [actions, promotable, promotionKey, selector, sessionNodeDid]);
  const control = (action: JobControlKind) => {
    if (!tool.toolCallDocId || !tool.nodeDid) return;
    actions
      .controlJob({ nodeDid: tool.nodeDid, toolCallDocId: tool.toolCallDocId, action })
      .catch((e: unknown) => toastFailure(action, e));
  };
  const answer = () => answerJob(tool.jobHandle);
  // The run view re-reads whenever the session's observation of this row
  // changes; the host bumps `revision` on every progress write.
  const observed = [
    state,
    progress?.revision,
    tool.statusKind,
    tool.completedAt,
    tool.jobOutcome?.code,
  ].join("|");
  return (
    <div
      className="grid gap-2 rounded-md bg-surface px-3 py-2"
      data-testid="job-progress"
    >
      <div className="flex flex-wrap items-center gap-2 text-sm">
        {state && <JobStatePill state={state} />}
        {progress && <span className="min-w-0 truncate">{progress.title}</span>}
        <span className="ml-auto flex gap-1">
          {state === "running" && (
            <>
              <Button
                size="sm"
                variant="outline"
                disabled={!tool.toolCallDocId || !tool.nodeDid}
                onClick={() => control("pause")}
              >
                Pause
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={!tool.toolCallDocId || !tool.nodeDid}
                onClick={() => control("stop")}
              >
                Stop
              </Button>
            </>
          )}
          {tool.toolCallDocId && tool.nodeDid && (
            <Button size="sm" variant="ghost" onClick={() => setRunOpen(true)}>
              Open run
            </Button>
          )}
        </span>
      </div>
      {progress && (
        <>
          <JobProgressBody state={state} progress={progress} />
          {state === "awaiting_user" && (
            <div className="flex items-center gap-2 text-sm">
              <p data-testid="job-question" className="min-w-0 flex-1">
                {progress.question}
              </p>
              <Button size="sm" variant="outline" onClick={answer}>
                Answer
              </Button>
            </div>
          )}
        </>
      )}
      {state && terminal && (
        <div className="flex items-center gap-2">
          <p data-testid="job-outcome" className="min-w-0 flex-1 text-sm">
            {tool.jobOutcome
              ? `${tool.jobOutcome.code}: ${tool.jobOutcome.message}`
              : words(state)}
          </p>
          {promotable && promotedAs.length === 0 && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => setPromotingKey(promotionKey)}
            >
              Promote
            </Button>
          )}
        </div>
      )}
      {promotingKey === promotionKey && promotable && (
        <PromoteWorkflowDialog
          key={promotionKey}
          selector={selector}
          onClose={(promoted) => {
            setPromotingKey(null);
            if (promoted) setPromotion({ key: promotionKey, names: [promoted.tool] });
          }}
        />
      )}
      {tool.toolCallDocId && tool.nodeDid && (
        <WorkflowRunSheet
          open={runOpen}
          nodeDid={tool.nodeDid}
          rowDocId={tool.toolCallDocId}
          observed={observed}
          onClose={() => setRunOpen(false)}
        />
      )}
      {!!tool.jobNextCalls?.length && (
        <ul data-testid="job-next-calls" className="grid gap-0.5 font-mono text-[11px]">
          {tool.jobNextCalls.map((call, i) => (
            <li key={i}>{call}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
