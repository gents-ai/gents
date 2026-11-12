/* The session panel's Workflows tab: the workflow runs this session started,
   read from its own rows, each opening to its stages, its agents and its
   controls. A run's card in the transcript opens it here. */

import { useCallback, useEffect, useMemo, useState } from "react";
import { createStore, useStore } from "zustand";
import { ArrowLeft, Route } from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { AsciiLoader } from "@gents/ui/components/ascii-loader";
import { Button } from "@gents/ui/components/button";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import type { SurfaceContext } from "@/app/surfaces";
import { dockScopeOf, workspace } from "@/app/workspace";
import { useSelectedSessionValue, useSessionFacts } from "@/hooks/useSelectedSession";
import { isTerminalJob, JobStateMark, JobStatePill, LiveAgents } from "./jobParts";
import { PanelEmpty } from "./PanelEmpty";
import { runTitle, useWorkflowRun, WorkflowRunView } from "./WorkflowRun";

export const WORKFLOWS_SURFACE = "workflows";

const isWorkflow = (tool: RenderedToolCallView) =>
  !!(tool.jobProgress || tool.jobState) && !!tool.toolCallDocId && !!tool.nodeDid;

/** The runs this session started, those still going first, then newest. */
export function useSessionWorkflows(sessionId: string | null) {
  const open = useSelectedSessionValue(
    (s) => !!sessionId && s?.sessionId === sessionId,
  );
  const tools = useSessionFacts()?.tools;
  return useMemo(() => {
    const runs = open && tools ? tools.filter(isWorkflow) : [];
    return [...runs].sort(
      (a, b) =>
        Number(isTerminalJob(a.jobState)) - Number(isTerminalJob(b.jobState)) ||
        (b.startedAt ?? "").localeCompare(a.startedAt ?? ""),
    );
  }, [open, tools]);
}

/* a run asked for from outside the tab, such as its card in the transcript,
   waiting for the tab to open it */
const asked = createStore<{ sessionId: string; run: string } | null>(() => null);

/** Opens the session panel's Workflows tab on one run. */
export function openWorkflowRun(sessionId: string, toolCallDocId: string) {
  asked.setState({ sessionId, run: toolCallDocId }, true);
  workspace.openSurface(dockScopeOf("session", sessionId), WORKFLOWS_SURFACE);
}

/* the run each session's tab had open, while the app runs */
const shownRuns = new Map<string, string | null>();

export function WorkflowsSurface({ sessionId }: SurfaceContext) {
  const runs = useSessionWorkflows(sessionId);
  const [held, setHeld] = useState(() => ({
    session: sessionId,
    run: sessionId ? (shownRuns.get(sessionId) ?? null) : null,
  }));
  if (held.session !== sessionId)
    setHeld({
      session: sessionId,
      run: sessionId ? (shownRuns.get(sessionId) ?? null) : null,
    });
  const show = useCallback(
    (run: string | null) => {
      if (sessionId) shownRuns.set(sessionId, run);
      setHeld({ session: sessionId, run });
    },
    [sessionId],
  );
  const wanted = useStore(asked, (a) =>
    a && a.sessionId === sessionId ? a.run : null,
  );
  useEffect(() => {
    if (!wanted) return;
    asked.setState(null, true);
    show(wanted);
  }, [wanted, show]);

  /* one run opens straight away; with several, the list comes first */
  const opened =
    runs.find((tool) => tool.toolCallDocId === held.run) ??
    (runs.length === 1 ? runs[0] : undefined);
  if (opened)
    return (
      <RunPane
        key={opened.toolCallDocId}
        tool={opened}
        onBack={runs.length > 1 ? () => show(null) : null}
      />
    );
  if (runs.length === 0)
    return (
      <PanelEmpty icon={Route} title="No workflows in this session">
        A workflow an agent runs in this session shows its stages here.
      </PanelEmpty>
    );
  return (
    <ScrollArea className="h-full">
      <ul data-testid="workflow-runs" className="grid gap-0.5 p-2">
        {runs.map((tool) => {
          const active = tool.jobProgress?.phases.find((p) => p.state === "active");
          return (
            <li key={tool.toolCallDocId}>
              <button
                type="button"
                data-testid="workflow-run-row"
                className="flex w-full min-w-0 items-center gap-3 rounded-md px-2 py-2 text-left text-sm hover:bg-accent"
                onClick={() => show(tool.toolCallDocId!)}
              >
                <JobStateMark state={tool.jobState} />
                <span className="min-w-0 flex-1 truncate font-medium">
                  {tool.jobProgress?.title || tool.jobHandle || "Workflow"}
                </span>
                {active ? (
                  <span className="max-w-[40%] truncate text-xs text-muted-foreground">
                    {active.title}
                  </span>
                ) : (
                  tool.jobState && <JobStatePill state={tool.jobState} />
                )}
              </button>
            </li>
          );
        })}
      </ul>
    </ScrollArea>
  );
}

function RunPane({
  tool,
  onBack,
}: {
  tool: RenderedToolCallView;
  onBack: (() => void) | null;
}) {
  /* the run view re-reads whenever the session's observation of the row
     changes; the host bumps `revision` on every progress write */
  const observed = [
    tool.jobState,
    tool.jobProgress?.revision,
    tool.statusKind,
    tool.completedAt,
    tool.jobOutcome?.code,
  ].join("|");
  const run = useWorkflowRun({
    nodeDid: tool.nodeDid!,
    rowDocId: tool.toolCallDocId!,
    observed,
    active: true,
  });
  return (
    <ScrollArea className="h-full">
      <div className="grid gap-3 px-3 py-2">
        <div className={cn("flex items-center gap-1", !onBack && "hidden")}>
          {onBack && (
            <Button
              variant="ghost"
              size="sm"
              className="-ml-1.5 h-7 gap-1 px-1.5"
              onClick={onBack}
            >
              <ArrowLeft className="size-3.5" />
              Back
            </Button>
          )}
        </div>
        <h2 className="font-heading text-base text-heading">{runTitle(run.view)}</h2>
        {/* Answer already writes and focuses the session's draft */}
        <WorkflowRunView run={run} onLeave={() => {}} />
      </div>
    </ScrollArea>
  );
}

/** Above the composer while this session has workflows going, like its
    goal: where the run stands without scrolling back to its card, and the
    way into its tab. */
export function WorkflowStrip({ sessionId }: { sessionId: string }) {
  const going = useSessionWorkflows(sessionId).filter(
    (tool) => !isTerminalJob(tool.jobState),
  );
  if (going.length === 0) return null;
  const one = going.length === 1 ? going[0]! : null;
  const waiting = going.some((tool) => tool.jobState === "awaiting_user");
  const phases = one?.jobProgress?.phases ?? [];
  const at = phases.findIndex((phase) => phase.state === "active");
  return (
    <button
      type="button"
      data-testid="workflow-strip"
      className={cn(
        "mb-2 flex w-full min-w-0 items-center gap-2 rounded-xl border border-border bg-raised px-3 py-2 text-left text-sm hover:bg-accent",
        waiting && "border-brand/40",
      )}
      onClick={() =>
        one
          ? openWorkflowRun(sessionId, one.toolCallDocId!)
          : workspace.openSurface(dockScopeOf("session", sessionId), WORKFLOWS_SURFACE)
      }
    >
      {one ? (
        <>
          <Route className="size-4 shrink-0 text-ink" />
          <span className="shrink-0 font-medium">
            {one.jobProgress?.title || one.jobHandle || "Workflow"}
          </span>
          {one.jobState === "awaiting_user" ? (
            <span className="min-w-0 truncate">Waiting on your answer</span>
          ) : (
            at >= 0 && (
              <span className="flex min-w-0 items-center gap-1.5 text-muted-foreground">
                {one.jobState === "running" && (
                  <AsciiLoader aria-hidden className="text-foreground" />
                )}
                <span className="min-w-0 truncate">
                  {phases[at]!.title} · stage {at + 1} of {phases.length}
                </span>
              </span>
            )
          )}
          {one.jobProgress && (
            <span className="ml-auto shrink-0">
              <LiveAgents active={one.jobProgress.active} />
            </span>
          )}
        </>
      ) : (
        <>
          <Route className="size-4 shrink-0 text-ink" />
          <span className="shrink-0 font-medium">{going.length} workflows running</span>
          <span className="min-w-0 truncate text-muted-foreground">
            {waiting
              ? "one is waiting on your answer"
              : going
                  .map((tool) => tool.jobProgress?.title || tool.jobHandle)
                  .join(", ")}
          </span>
        </>
      )}
    </button>
  );
}
