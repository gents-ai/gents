import { useCallback, useMemo, useRef, useState, type ComponentProps } from "react";
import { toast } from "sonner";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { useApp, useView } from "@/app/AppContext";
import { PanelMenu } from "@/app/PanelMenu";
import { PaneBar } from "@/app/PaneBar";
import { Slot } from "@/contrib/react/slot";
import { SESSION_HEADER_ACTIONS_AREA } from "@/contrib/types";
import {
  useHomeDid,
  useInterruptVisible,
  useSelectedNode,
  useSelectedNodeDid,
  useSessionLoad,
} from "@/hooks/useClient";
import { toastFailure } from "@/lib/failure";
import { isWorkingNode } from "@/lib/nodes";
import { useScroller } from "@/lib/scroll";
import { useSentTurnPlacement } from "@/preferences";
import { listedSession, nodeOf } from "../../../hooks/fleetStore";
import { useFleet, workersOfId } from "../../hooks/useFleet";
import {
  useSelectedSession,
  useSelectedSessionFields,
} from "../../hooks/useSelectedSession";
import { interruptTarget, isStopping } from "../activity-status";
import { agentName } from "../behavior";
import { NodeAgentStack } from "../NodeBehaviorStack";
import { useParentWork } from "../parentWork";
import { SessionContext } from "../SessionContextMeter";
import { StartedSessionsButton } from "../StartedSessionsButton";
import { ParentLine, StartedByAutomation, Title } from "../SessionHeader";
import { SessionLoading } from "../SessionLoading";
import { useComposerRoom, useHeaderScrolledOut } from "../sessionLayout";
import { sentTurnKey } from "../stream-reveal";
import { TranscriptPanel, type TranscriptOwner } from "../Transcript";
import { type WorkerActions } from "../WorkerActions";
import { useSessionProvenance, useWorkers } from "../workers";
import { ProviderRecovery } from "./ProviderRecovery";
import { OpenSessionComposer } from "./OpenSessionComposer";

export function OpenSession() {
  const activeRequestId = useView((view) => view.shellProjection.activeRequestId);
  const {
    stores,
    actions: { interruptRequest, renameSession, loadSessionContextDetails },
  } = useApp();
  const inFlight = useInterruptVisible();
  const selectedNodeDid = useSelectedNodeDid();
  const deployment = useSelectedNode();
  const selectedSessionId = stores.selection.use.sessionId();
  const sessionLoad = useSessionLoad();
  const session = useSelectedSessionFields(selectScreenFacts);
  const loadContextDetails = useCallback(
    (input: NonNullable<DesktopSessionSnapshot["context"]["lastRequest"]>) => {
      if (!session?.nodeDid) return Promise.resolve(null);
      return loadSessionContextDetails({
        nodeDid: session.nodeDid,
        sessionId: session.sessionId,
        requestId: input.requestId,
        callId: input.callId,
      });
    },
    [loadSessionContextDetails, session?.nodeDid, session?.sessionId],
  );
  const homeDid = useHomeDid();
  const sessionWorkers = useFleet((s) => workersOfId(s, session?.sessionId));
  const [requestedStop, setRequestedStop] = useState<string | null>(null);
  const column = useRef<HTMLDivElement | null>(null);
  const [scroller, ownScroller] = useScroller();
  const columnRef = useCallback(
    (element: HTMLDivElement | null) => {
      column.current = element;
      ownScroller(element);
    },
    [ownScroller],
  );
  /* the transcript owns the scroll position; the composer needs its foot */
  const [owner, setOwner] = useState<TranscriptOwner>(NO_OWNER);
  const { atBottom, toBottom, settle } = owner;
  const anchorSentTurn = useSentTurnPlacement() === "anchor";
  const sentTurn = useSentTurnKey(selectedSessionId, session?.latestRequestId ?? null);
  /* once the full header scrolls out, a condensed one sticks to the top */
  const [condensed, headerEnd] = useHeaderScrolledOut(scroller);
  const provenance = useSessionProvenance();
  const workers = useWorkers(provenance);
  const parentWork = useParentWork(provenance);
  const composer = useComposerRoom(column, settle);
  const workerActions = useMemo<WorkerActions>(
    () => ({
      interrupt: (request) => {
        interruptRequest({
          requestId: request.requestId,
          nodeDid: request.nodeDid,
        }).catch((e: unknown) => toastFailure("stop", e));
      },
    }),
    [interruptRequest],
  );
  const summary = useFleet((s) =>
    listedSession(s, deployment?.nodeDid, session?.sessionId),
  );
  const sessionNode = useFleet((s) => nodeOf(s, session?.nodeDid));

  const stoppableRequestId = interruptTarget({
    trackedRequestId: activeRequestId,
    latestRequestId: session?.latestRequestId ?? null,
    turnState: session?.turnState ?? null,
  });
  const stopping = isStopping({
    inFlight,
    requestId: stoppableRequestId,
    latestRequestId: session?.latestRequestId ?? null,
    interruptObserved:
      session?.latestRequestOutcome?.cancelCause?.source === "requestInterrupt",
    requestedStop,
  });
  const stop = async () => {
    const requestId = stoppableRequestId;
    if (!requestId || stopping) return;
    setRequestedStop(requestId);
    const release = () =>
      setRequestedStop((current) => (current === requestId ? null : current));
    try {
      await interruptRequest({ requestId, nodeDid: selectedNodeDid });
    } catch (e) {
      release();
      toastFailure("stop", e);
    }
  };

  if (!session && sessionLoad.phase !== "failed" && sessionLoad.found !== false)
    return <SessionLoading />;

  return (
    <div
      className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)]"
      data-testid="session-screen"
    >
      <div className="relative flex min-h-0 min-w-0 flex-col">
        <div
          aria-hidden="true"
          data-testid="transcript-top-fade"
          className={cn(
            "pointer-events-none absolute inset-x-0 top-0 z-20 h-8 bg-gradient-to-b from-background to-transparent transition-opacity duration-200",
            condensed ? "opacity-100" : "opacity-0",
          )}
        />
        <div ref={columnRef} className="min-h-0 flex-1">
          <ScrollArea className="h-full">
            <div
              data-transcript-content
              className={cn(
                "mx-auto flex min-h-full w-full max-w-page flex-col px-6 pt-4",
                parentWork.hasSenders && "sm:pr-14",
              )}
            >
              <PaneBar>
                <div
                  aria-hidden={!condensed}
                  data-testid="pane-bar-condensed"
                  className={cn(
                    "flex min-w-0 flex-1 items-center gap-2 transition-[opacity,transform] duration-150 ease-out",
                    condensed
                      ? "translate-y-0 opacity-100"
                      : "pointer-events-none translate-y-1 opacity-0",
                  )}
                >
                  <NodeAgentStack
                    nodeDid={session?.nodeDid}
                    agentId={session?.agentId}
                    size="sm"
                    keyboard
                    workers={sessionWorkers}
                  />
                  <div className="min-w-0 flex-1 [&_form]:min-w-0 [&_h1]:truncate [&_h1]:text-sm [&_input]:h-7 [&_input]:text-sm">
                    {session ? (
                      <Title
                        key={session.sessionId + (session.title ?? "")}
                        title={session.title ?? "Untitled session"}
                        onRename={async (title) => {
                          await renameSession(session.sessionId, title);
                          toast("Renamed");
                        }}
                      />
                    ) : (
                      <h1 className="font-heading text-sm font-medium text-heading">
                        Session
                      </h1>
                    )}
                  </div>
                </div>
                {session && (
                  <StartedSessionsButton
                    sessionId={session.sessionId}
                    className="max-md:hidden"
                  />
                )}
                {session?.context && (
                  <div
                    aria-hidden={!condensed}
                    className={cn(
                      "grid shrink-0 transition-[grid-template-columns,margin,opacity,transform] duration-150 ease-out",
                      condensed
                        ? "ml-0 translate-y-0 grid-cols-[1fr] opacity-100"
                        : "pointer-events-none -ml-2 translate-y-1 grid-cols-[0fr] opacity-0",
                    )}
                  >
                    <div className="flex min-w-0 items-center overflow-hidden">
                      <SessionContext
                        context={session.context}
                        loadDetails={loadContextDetails}
                        compact
                      />
                    </div>
                  </div>
                )}
                <Slot area={SESSION_HEADER_ACTIONS_AREA} />
                <PanelMenu routeName="session" />
              </PaneBar>
              <div className="flex items-start justify-between gap-4">
                <div>
                  {session ? (
                    <Title
                      key={session.sessionId + (session.title ?? "")}
                      title={session.title ?? "Untitled session"}
                      onRename={async (title) => {
                        await renameSession(session.sessionId, title);
                        toast("Renamed");
                      }}
                    />
                  ) : (
                    <h1 className="font-heading text-lg font-medium text-heading">
                      Session
                    </h1>
                  )}
                  {parentWork.parent && (
                    <div className="mt-1 mb-1">
                      <ParentLine work={parentWork} />
                    </div>
                  )}
                  {summary &&
                    !parentWork.parent &&
                    (summary.taskId || summary.triggerId) && (
                      <div className="mt-1 mb-1">
                        <StartedByAutomation
                          summary={summary}
                          nodeDid={deployment?.nodeDid ?? null}
                        />
                      </div>
                    )}
                  <div className="mt-3 flex flex-wrap items-center gap-2">
                    <NodeAgentStack
                      nodeDid={session?.nodeDid}
                      agentId={session?.agentId}
                      workers={sessionWorkers}
                    />
                    <span className="text-sm text-muted-foreground">
                      {agentName(session?.agentId ?? null, deployment)}
                      {sessionNode && !isWorkingNode(sessionNode, homeDid)
                        ? ` on ${sessionNode.node.displayName ?? sessionNode.label}`
                        : null}
                    </span>
                    {session?.context && (
                      <SessionContext
                        context={session.context}
                        loadDetails={loadContextDetails}
                      />
                    )}
                  </div>
                </div>
              </div>

              <div ref={headerEnd} aria-hidden="true" />
              {session?.nodeDid && session.latestRequestId && (
                <ProviderRecovery
                  nodeDid={session.nodeDid}
                  sessionId={session.sessionId}
                  requestId={session.latestRequestId}
                  turnState={session.turnState}
                />
              )}
              <SelectedTranscript
                inFlight={inFlight}
                stopping={stopping}
                scroller={scroller}
                sentTurn={sentTurn}
                anchorSentTurn={anchorSentTurn}
                onOwner={setOwner}
                workers={workers}
                parentWork={parentWork}
                workerActions={workerActions}
              />
              <OpenSessionComposer
                roomRef={composer}
                atBottom={atBottom}
                toBottom={toBottom}
                stop={stop}
                stopping={stopping}
                goal={session?.goal ?? null}
                agentId={session?.agentId ?? null}
              />
            </div>
          </ScrollArea>
        </div>
      </div>
    </div>
  );
}

const NO_OWNER: TranscriptOwner = {
  atBottom: true,
  toBottom: () => {},
  settle: () => {},
};

function SelectedTranscript(
  props: Omit<ComponentProps<typeof TranscriptPanel>, "session">,
) {
  return <TranscriptPanel {...props} session={useSelectedSession()} />;
}

type ScreenFacts = Pick<
  DesktopSessionSnapshot,
  | "sessionId"
  | "nodeDid"
  | "agentId"
  | "title"
  | "context"
  | "goal"
  | "latestRequestId"
  | "turnState"
  | "latestRequestOutcome"
>;

function selectScreenFacts(session: DesktopSessionSnapshot | null): ScreenFacts | null {
  if (!session) return null;
  return {
    sessionId: session.sessionId,
    nodeDid: session.nodeDid,
    agentId: session.agentId,
    title: session.title,
    context: session.context,
    goal: session.goal,
    latestRequestId: session.latestRequestId,
    turnState: session.turnState,
    latestRequestOutcome: session.latestRequestOutcome,
  };
}

/* The turn the view goes to after a send from here, by key. The session's
   folds of what was sent are read as one string, so a snapshot that
   changes nothing about them re-renders nothing here. */
function useSentTurnKey(sessionId: string | null, latestRequestId: string | null) {
  const sentTurns = useView((view) => view.sentTurns);
  const { folds } = useSelectedSessionFields((held) => ({
    folds: JSON.stringify(
      (held?.foldedInputs ?? [])
        .filter((input) => sentTurns.some((turn) => turn.requestId === input.requestId))
        .map((input) => [input.requestId, input.foldedIntoRequestId]),
    ),
  }));
  return useMemo(
    () =>
      sentTurnKey(
        sentTurns,
        sessionId,
        latestRequestId,
        (JSON.parse(folds) as [string, string][]).map(
          ([requestId, foldedIntoRequestId]) => ({
            requestId,
            foldedIntoRequestId,
          }),
        ),
      ),
    [sentTurns, sessionId, latestRequestId, folds],
  );
}
