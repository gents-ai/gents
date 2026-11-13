/* A background job's row: where it stands, read from the fields the bridge
   carries for any job plugin. The state is the row's own (`jobState`), never
   the progress record's, which stays "running" on an ended row. The next
   calls are the bridge's, printed as given. */

import { useEffect, useState } from "react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import type { JobControlKind } from "@source-inc/gents-desktop-client/generated/JobControlKind";
import { PanelRightOpen, Pause, Square } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Hint } from "./Hint";
import { toastFailure } from "../lib/failure";
import { useApp } from "../app/AppContext";
import { PromoteWorkflowDialog } from "./PromoteWorkflowDialog";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { useHomeDid } from "../hooks/useClient";
import { isLocalNode } from "../lib/firstRun";
import { WorkflowRunSheet } from "./WorkflowRunSheet";
import { getSurface } from "@/app/surfaces";
import { openWorkflowRun, WORKFLOWS_SURFACE } from "./WorkflowsSurface";
import {
  isTerminalJob,
  JobProgressBody,
  JobStateMark,
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
  const observed = [
    state,
    progress?.revision,
    tool.statusKind,
    tool.completedAt,
    tool.jobOutcome?.code,
  ].join("|");
  return (
    <div
      className="grid gap-2 rounded-lg bg-accent/40 px-3 py-2.5"
      data-testid="job-progress"
    >
      <div className="flex min-w-0 items-center gap-3 text-sm">
        <JobStateMark state={state} />
        <span className="min-w-0 truncate font-medium">
          {progress?.title || tool.jobHandle || "Workflow"}
        </span>
        {state && state !== "running" && <JobStatePill state={state} />}
        <span className="ml-auto flex shrink-0 items-center gap-0.5">
          {state === "running" && (
            <>
              <Hint label="Pause">
                <Button
                  size="icon-xs"
                  variant="quiet"
                  disabled={!tool.toolCallDocId || !tool.nodeDid}
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
                  disabled={!tool.toolCallDocId || !tool.nodeDid}
                  aria-label="Stop"
                  onClick={() => control("stop")}
                >
                  <Square />
                </Button>
              </Hint>
            </>
          )}
          {tool.toolCallDocId && tool.nodeDid && (
            <Button
              size="xs"
              variant="quiet"
              onClick={() =>
                sessionId && tool.toolCallDocId && getSurface(WORKFLOWS_SURFACE)
                  ? openWorkflowRun(sessionId, tool.toolCallDocId, tool.nodeDid!)
                  : setRunOpen(true)
              }
            >
              <PanelRightOpen />
              Open run
            </Button>
          )}
        </span>
      </div>
      <div className="grid gap-2 pl-7">
        {progress && (
          <>
            <JobProgressBody state={state} progress={progress} nodeDid={tool.nodeDid} />
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
      </div>
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
