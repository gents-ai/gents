/* One session: start a new one, or read and continue an existing one.
   Built from the kit's conversation patterns over the bridge's session
   projection: the timeline items are its own
   RenderedTimelineItem, rendered as they arrive. */
import { placeholderFor } from "@/lib/send-status";
import {
  Fragment,
  useCallback,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
} from "react";
import { ArrowDown, ChevronDown } from "lucide-react";
import { toast } from "sonner";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import type { SendStatus } from "@source-inc/gents-desktop-chat";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { Composer } from "@gents/ui/conversation";
import type { ShellView } from "@/../hooks/shellView";
import { ChatFolderPicker } from "./ChatFolderPicker";
import { useFollowTail, useScroller } from "@/lib/scroll";
import { useComposerRoom, useHeaderScrolledOut } from "./sessionLayout";
import { href, navigate } from "@/lib/router";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";

import { ScrollArea } from "@gents/ui/components/scroll-area";
import { behaviorName } from "./behavior";
import { AgentAvatar } from "./AgentAvatar";
import { PanelMenu } from "@/app/PanelMenu";
import { PaneBar } from "@/app/PaneBar";
import { BehaviorPicker } from "./BehaviorPicker";
import { LoadingStatus } from "./LoadingStatus";
import { SlashSkillMenu } from "./SlashSkillMenu";
import { useSlashSkills } from "./useSlashSkills";
import { isStopping } from "./activity-status";
import { AccessSentence } from "./parts";
import { NodeBehaviorStack } from "./NodeBehaviorStack";
import { isWorkingNode } from "@/lib/nodes";
import { SessionLoading } from "./SessionLoading";
import { SubagentList } from "./WorkerStep";
import { useSessionProvenance, useWorkers } from "./workers";
import { useParentWork } from "./parentWork";
import { type WorkerActions } from "./WorkerActions";
import { ReplyingTo } from "./ReplyingTo";
import { workspace } from "@/app/workspace";
import { toastFailure } from "@/lib/failure";
import {
  useSelectedSession,
  useSelectedSessionFields,
} from "../hooks/useSelectedSession";
import { useDraft } from "../../hooks/draftStore";
import { useShallow } from "zustand/react/shallow";
import { useFleet, workersOfId } from "../hooks/useFleet";
import { listedSession, nodeOf } from "../../hooks/fleetStore";
import { fleetNodes } from "@/lib/scope";
import { agentOf } from "@/lib/agents";
import { useApp, useView } from "@/app/AppContext";
import {
  useChatFolder,
  useHomeDid,
  useInterruptVisible,
  useMailboxCause,
  useSelectedAgentDid,
  useNodeCount,
  useSelectedBehaviorId,
  useSelectedNode,
  useSessionLoad,
} from "@/hooks/useClient";
import { SessionContext } from "./SessionContextMeter";
import { ParentLine, StartedByAutomation, Title, Goal } from "./SessionHeader";
import { TranscriptPanel } from "./Transcript";

/** Add only local composer emptiness; every other blocker belongs to ClientShell. */
export function presentedComposerSendStatus(
  draft: string,
  canonicalNonEmptyStatus: SendStatus,
): SendStatus {
  return draft.trim()
    ? canonicalNonEmptyStatus
    : {
        kind: "disabled",
        reason: "composerEmpty",
        hint: "Type a message to send",
      };
}

export function useBehaviorChoice() {
  const selectedBehaviorId = useSelectedBehaviorId();
  const { selectBehavior } = useApp().actions;
  return {
    // Read the same effective selection that owns composer admission. Defaults,
    // mailbox routing, and agent changes are resolved by the selection, not here.
    behaviorId: selectedBehaviorId,
    setPicked: selectBehavior,
  };
}

/* the node a new chat starts on, where there is a choice: the name is the
   button's own text, so the heading still reads as one sentence */
function NodeChoice({
  name,
  selectedAgentDid,
  onSelect,
}: {
  name: string;
  selectedAgentDid: string | null;
  onSelect: (agentDid: string) => void;
}) {
  const nodes = useFleet(useShallow(fleetNodes));
  const homeDid = useHomeDid();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <button
            type="button"
            title="Choose a node"
            className="inline-flex items-center gap-1 underline decoration-border underline-offset-4 hover:decoration-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm"
          />
        }
      >
        {name}
        <ChevronDown className="size-4 opacity-50" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-56">
        <DropdownMenuRadioGroup value={selectedAgentDid ?? ""} onValueChange={onSelect}>
          {nodes.map((n, i, all) => (
            <Fragment key={n.agentDid}>
              {/* a faint line between the local node and the paired ones */}
              {i > 0 &&
                isWorkingNode(all[i - 1]!, homeDid) &&
                !isWorkingNode(n, homeDid) && (
                  <DropdownMenuSeparator className="opacity-60" />
                )}
              <DropdownMenuRadioItem value={n.agentDid} disabled={!n.dialSucceeded}>
                <AgentAvatar
                  name={n.agentPrincipal.displayName ?? n.label}
                  className="size-5 text-[9px]"
                />
                <span className="min-w-0 flex-1 truncate">
                  {n.agentPrincipal.displayName ?? n.label}
                </span>
                {isWorkingNode(n, homeDid) && (
                  <span className="text-xs text-muted-foreground">local</span>
                )}
              </DropdownMenuRadioItem>
            </Fragment>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Display the existing workflow owner's observation, never infer queue health. */
/* The line under the composer holds its place whether or not it has
   anything to say: a status arriving and leaving moved the composer up
   and down, and a person's eye with it. One line of the small size is
   always reserved; an error, being rarer and longer, may still grow it. */
export function SessionSubmissionStatus({
  activityStatus,
  hint,
  reserve = true,
}: {
  activityStatus: ShellView["shellProjection"]["activityStatus"];
  hint?: string | null;
  /** a new chat has no transcript above to hold still, so its line may
      take no room until it has something to say */
  reserve?: boolean;
}) {
  const status = activityStatus;
  const quiet = !status && hint;
  const empty = !status && !quiet;
  return (
    <div
      className={cn("px-1", reserve ? "mt-2 min-h-4" : !empty && "mt-2")}
      data-testid="composer-status"
    >
      {status && (
        <div
          role="status"
          title={status.detail}
          className="text-xs leading-4 text-muted-foreground animate-in fade-in-0 duration-200 motion-reduce:animate-none"
        >
          <span>{status.label}</span>
        </div>
      )}
      {quiet && <p className="text-xs leading-4 text-muted-foreground">{hint}</p>}
    </div>
  );
}

/* an earlier request's failure, shown where it happened; the session has
   moved on to a later request, so there is nothing to retry here */

export function SessionScreen() {
  const activeRequestId = useView((view) => view.shellProjection.activeRequestId);
  const activityStatus = useView((view) => view.shellProjection.activityStatus);
  const {
    drafts,
    stores,
    actions: {
      acceptsComposeIntent,
      captureComposeIntent,
      interruptRequest,
      renameSession,
      selectAgent,
      sendMessage,
      setChatFolder,
    },
  } = useApp();
  const chatFolder = useChatFolder();
  const nodeCount = useNodeCount();
  const draftKey = useView((view) => view.draftKey);
  const inFlight = useInterruptVisible();
  const mailboxCause = useMailboxCause();
  const sendStatus = useView((view) => view.shellProjection.nonEmptyContentSendStatus);
  const selectedAgentDid = useSelectedAgentDid();
  const deployment = useSelectedNode();
  const selectedSessionId = stores.selection.use.sessionId();
  const sending = stores.chat.use.sending();
  const sessionLoad = useSessionLoad();
  const session = useSelectedSessionFields(selectScreenFacts);
  const homeDid = useHomeDid();
  const sessionWorkers = useFleet((s) => workersOfId(s, session?.sessionId));
  const [draft, setDraft] = useDraft(drafts, draftKey);
  const [requestedStop, setRequestedStop] = useState<string | null>(null);
  /* the transcript column follows new content while the reader is near
     the bottom; a reader who has scrolled up is left where they are */
  const column = useRef<HTMLDivElement | null>(null);
  const [scroller, ownScroller] = useScroller();
  const [content, setContent] = useState<HTMLDivElement | null>(null);
  const columnRef = useCallback(
    (element: HTMLDivElement | null) => {
      column.current = element;
      ownScroller(element);
    },
    [ownScroller],
  );
  /* away from the bottom, a button offers the way back; scrolling is the cue */
  const { atBottom, toBottom, settle } = useFollowTail(scroller, selectedSessionId);
  /* once the full header scrolls out, a condensed one sticks to the top */
  const [condensed, headerEnd] = useHeaderScrolledOut(scroller);
  const choice = useBehaviorChoice();
  const provenance = useSessionProvenance();
  const workers = useWorkers(provenance);
  const parentWork = useParentWork(provenance);
  const composer = useComposerRoom(column, settle);
  /* a person stopping a subagent from here: the canonical interrupt of the
     one request that row's call caused; the row settles when that request
     is terminal */
  const workerActions = useMemo<WorkerActions>(
    () => ({
      interrupt: (request) => {
        interruptRequest({
          requestId: request.requestId,
          agentDid: request.agentDid,
        }).catch((e: unknown) => toastFailure("stop", e));
      },
    }),
    [interruptRequest],
  );
  /* the principal name, or the pairing label while a paired node has not
     replicated its principal yet */
  const agentName =
    deployment?.agentPrincipal.displayName ?? deployment?.label ?? "the agent";
  /* the snapshot says what happened in a session; the summary says where it
     came from, which is the list's own view of it */
  const summary = useFleet((s) =>
    listedSession(s, deployment?.agentDid, session?.sessionId),
  );
  const sessionNode = useFleet((s) => nodeOf(s, session?.agentDid));

  const send = async (text: string) => {
    const pending = sendMessage(text, session?.behaviorId ?? choice.behaviorId);
    const intentGeneration = captureComposeIntent();
    const result = await pending;
    if (result) setDraft((current) => (current === text ? "" : current));
    if (!acceptsComposeIntent(intentGeneration)) return;
    if (result && result.sessionId !== selectedSessionId) {
      if (!selectedSessionId) workspace.adoptNewSessionDock(result.sessionId);
      navigate({ name: "session", sessionId: result.sessionId });
    }
  };

  const contextFor = (behaviorId?: string | null) => {
    const b = agentOf(deployment, behaviorId);
    return deployment?.contexts.find((c) => c.context_id === b?.contextId);
  };
  const startSlash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    contextFor(choice.behaviorId),
  );
  const slash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    contextFor(session?.behaviorId),
  );

  /* ---- start a new session ---- */
  if (!selectedSessionId) {
    const env = deployment?.behaviorEnvironments.find(
      (e) => e.behaviorId === choice.behaviorId,
    );
    const chosenName = behaviorName(choice.behaviorId, deployment);
    const startStatus = presentedComposerSendStatus(draft, sendStatus);
    return (
      <div
        key={selectedAgentDid ?? "new"}
        data-testid="session-screen"
        className="mx-auto grid min-h-full max-w-2xl content-center gap-6 px-6 py-16 animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none"
      >
        <div className="flex items-start gap-3">
          <AgentAvatar name={agentName} className="size-8" />
          <div>
            <h1 className="font-heading text-lg font-medium text-heading">
              Start a new chat with{" "}
              {nodeCount > 1 ? (
                <NodeChoice
                  name={agentName}
                  selectedAgentDid={selectedAgentDid}
                  onSelect={selectAgent}
                />
              ) : (
                agentName
              )}
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              {mailboxCause
                ? "Answering a mailbox item; the first message starts its request"
                : "The first message creates the session automatically"}
            </p>
          </div>
        </div>
        <div data-testid="composer">
          <Composer
            value={draft}
            onChange={setDraft}
            onSend={send}
            models={[]}
            disabled={sendStatus.kind === "disabled"}
            above={
              <>
                <ReplyingTo />
                <SlashSkillMenu
                  items={startSlash.items}
                  active={startSlash.active}
                  onPick={startSlash.accept}
                />
              </>
            }
            onKeyDown={startSlash.onKeyDown}
            leading={
              <>
                <BehaviorPicker
                  deployment={deployment}
                  behaviorId={choice.behaviorId}
                  onChange={choice.setPicked}
                />
                <ChatFolderPicker folder={chatFolder} onChange={setChatFolder} />
              </>
            }
            sending={sending}
            placeholder={placeholderFor(startStatus, "Ask anything")}
          />
          {/* inside the composer's row, so the grid's gap is not paid twice
              around a line that is usually empty */}
          <SessionSubmissionStatus activityStatus={activityStatus} reserve={false} />
        </div>
        <p className="text-xs text-muted-foreground">
          <AccessSentence name={chosenName} env={env} />
          {deployment && choice.behaviorId && (
            <>
              {" "}
              <a
                href={href({
                  name: "agent",
                  agentDid: deployment.agentDid,
                  section: "behaviors",
                  item: choice.behaviorId,
                })}
                className="underline decoration-border underline-offset-4 hover:text-foreground"
              >
                Configure
              </a>
            </>
          )}
        </p>
      </div>
    );
  }

  /* ---- an existing session ---- */

  /* stop: the interrupt reaches this request only; sessions it started keep
     their own work, each stoppable from its row or its own screen */
  const stoppableRequestId = activeRequestId ?? session?.latestRequestId ?? null;
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
      await interruptRequest({ requestId, agentDid: selectedAgentDid });
    } catch (e) {
      release();
      toastFailure("stop", e);
    }
  };

  /* Local text plus the canonical shell admission decision. */
  const status = presentedComposerSendStatus(draft, sendStatus);
  const queueing = status.kind === "queue";

  /* Until the session is here, nothing of its screen is. Drawn without it,
     the screen assembled under the reader's eye — chrome, then a title
     reading "loading", then the transcript — and changed shape as each
     part landed. One mark, centred, says it is coming. A load that failed,
     or a session the store does not have, keeps the screen: its
     LoadingStatus says what happened and offers the way on. */
  if (!session && sessionLoad.phase !== "failed" && sessionLoad.found !== false)
    return <SessionLoading />;

  return (
    <div
      className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)]"
      data-testid="session-screen"
    >
      <div className="relative flex min-h-0 min-w-0 flex-col">
        {/* once the header has scrolled out, a fade at the top says the
            transcript continues above, where the condensed title now is */}
        <div
          aria-hidden="true"
          data-testid="transcript-top-fade"
          className={cn(
            "pointer-events-none absolute inset-x-0 top-0 z-20 h-8 bg-gradient-to-b from-background to-transparent transition-opacity duration-200",
            condensed ? "opacity-100" : "opacity-0",
          )}
        />
        {/* transcript and composer scroll together in the kit's scroll area;
            the composer sticks to the foot so the bar runs the full height */}
        <div ref={columnRef} className="min-h-0 flex-1">
          <ScrollArea className="h-full">
            {/* the right gutter is the sender marks' column, so it is only
                  spent where marks can appear: a session another sent to, on a
                  screen wide enough to give the width away. Elsewhere the
                  column keeps its even padding. */}
            <div
              ref={setContent}
              className={cn(
                "mx-auto flex min-h-full w-full max-w-page flex-col px-6 pt-4",
                parentWork.hasSenders && "sm:pr-14",
              )}
            >
              <PaneBar>
                {/* the header's facts, once the header has scrolled out: they
                    ease in where the reader's eye already is, and the bar's
                    height never changes, so nothing moves */}
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
                  <NodeBehaviorStack
                    nodeDid={session?.agentDid}
                    behaviorId={session?.behaviorId}
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
                      /* no session to name: the LoadingStatus below says what
                       happened, so the title stays neutral */
                      <h1 className="font-heading text-sm font-medium text-heading">
                        Session
                      </h1>
                    )}
                  </div>
                </div>
                {/* the meter rides up with the header, next to the menu */}
                {session?.context && (
                  <div
                    aria-hidden={!condensed}
                    className={cn(
                      "flex shrink-0 items-center transition-[opacity,transform] duration-150 ease-out",
                      condensed
                        ? "translate-y-0 opacity-100"
                        : "pointer-events-none translate-y-1 opacity-0",
                    )}
                  >
                    <SessionContext context={session.context} compact />
                  </div>
                )}
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
                    /* no session to name: the LoadingStatus below says what
                       happened, so the title stays neutral */
                    <h1 className="font-heading text-lg font-medium text-heading">
                      Session
                    </h1>
                  )}
                  {parentWork.parent && (
                    <div className="mt-1 mb-1">
                      <ParentLine work={parentWork} />
                    </div>
                  )}
                  {workers.all.length > 0 && (
                    <div className="mt-1 mb-1">
                      <SubagentList workers={workers} />
                    </div>
                  )}
                  {summary &&
                    !parentWork.parent &&
                    (summary.taskId || summary.triggerId) && (
                      <div className="mt-1 mb-1">
                        <StartedByAutomation
                          summary={summary}
                          agentDid={deployment?.agentDid ?? null}
                        />
                      </div>
                    )}
                  <div className="mt-3 flex flex-wrap items-center gap-2">
                    <NodeBehaviorStack
                      nodeDid={session?.agentDid}
                      behaviorId={session?.behaviorId}
                      workers={sessionWorkers}
                    />
                    <span className="text-sm text-muted-foreground">
                      {behaviorName(session?.behaviorId ?? null, deployment)}
                      {/* the node only when it is not the local one, as the
                          marks beside it do */}
                      {sessionNode && !isWorkingNode(sessionNode, homeDid)
                        ? ` on ${sessionNode.agentPrincipal.displayName ?? sessionNode.label}`
                        : null}
                    </span>
                    {session?.context && <SessionContext context={session.context} />}
                  </div>
                </div>
              </div>

              <div ref={headerEnd} aria-hidden="true" />
              <SelectedTranscript
                inFlight={inFlight}
                stopping={stopping}
                scroller={scroller}
                content={content}
                workers={workers}
                parentWork={parentWork}
                workerActions={workerActions}
              />
              {/* its height is published as --composer-h, so a block that
                  sticks to the foot of the scroller can sit clear of it
                  rather than hard-coding what the composer happens to be */}
              {/* the composer owns the foot of the scroller: anything else
                  that sticks there passes behind it rather than over it,
                  however short the block it belongs to turns out to be */}
              <div
                ref={composer}
                className="sticky bottom-0 z-20 mt-auto bg-background pt-6 pb-6"
              >
                {/* a goal outlives the turns under it, so it sits with the
                    next one rather than at the top where it scrolls away */}
                {session?.goal && <Goal goal={session.goal} />}
                {/* conversation still running on under the composer: a short
                    fade on its top edge says the transcript has not ended,
                    where a hard edge reads as the end of it */}
                <div
                  aria-hidden
                  className={cn(
                    "pointer-events-none absolute inset-x-0 bottom-full h-8 bg-gradient-to-t from-background to-transparent transition-opacity duration-200",
                    atBottom ? "opacity-0" : "opacity-100",
                  )}
                />
                {/* the way back sits on the footer's top edge, whatever the footer holds */}
                {!atBottom && (
                  <Button
                    variant="raised"
                    size="icon-sm"
                    aria-label="Back to bottom"
                    onClick={toBottom}
                    className="absolute top-0 left-1/2 z-10 -translate-x-1/2 -translate-y-1/2 rounded-full shadow-md"
                  >
                    <ArrowDown />
                  </Button>
                )}
                <LoadingStatus />
                <div data-testid="composer">
                  <Composer
                    value={draft}
                    onChange={setDraft}
                    onSend={send}
                    models={[]}
                    disabled={sendStatus.kind === "disabled" && !inFlight}
                    above={
                      <>
                        <ReplyingTo />
                        <SlashSkillMenu
                          items={slash.items}
                          active={slash.active}
                          onPick={slash.accept}
                        />
                      </>
                    }
                    onKeyDown={slash.onKeyDown}
                    leading={
                      <ChatFolderPicker folder={chatFolder} onChange={setChatFolder} />
                    }
                    sending={sending || (inFlight && !queueing)}
                    onStop={inFlight && !stopping ? stop : undefined}
                    placeholder={
                      inFlight
                        ? sendStatus.kind === "queue"
                          ? "Add a message; it waits for this turn"
                          : "Ask anything"
                        : placeholderFor(status, "Ask anything")
                    }
                  />
                </div>
                {/* the placeholder already says why sending is off while the
                    box is empty; the hint is only for when typed text hides it */}
                <SessionSubmissionStatus
                  activityStatus={activityStatus}
                  hint={
                    status.kind === "disabled" && !inFlight && draft.trim() !== ""
                      ? status.hint
                      : null
                  }
                />
              </div>
            </div>
          </ScrollArea>
        </div>
      </div>
    </div>
  );
}

/* The transcript is the one reader of every streamed chunk: it alone selects
   the whole session, so a chunk re-renders it and not the screen around it. */
function SelectedTranscript(
  props: Omit<ComponentProps<typeof TranscriptPanel>, "session">,
) {
  return <TranscriptPanel {...props} session={useSelectedSession()} />;
}

/* What the screen around the transcript reads from the session. A streamed
   chunk keeps each of these, so it reaches the transcript and not the screen. */
type ScreenFacts = Pick<
  DesktopSessionSnapshot,
  | "sessionId"
  | "agentDid"
  | "behaviorId"
  | "title"
  | "context"
  | "goal"
  | "latestRequestId"
  | "latestRequestOutcome"
>;

function selectScreenFacts(session: DesktopSessionSnapshot | null): ScreenFacts | null {
  if (!session) return null;
  return {
    sessionId: session.sessionId,
    agentDid: session.agentDid,
    behaviorId: session.behaviorId,
    title: session.title,
    context: session.context,
    goal: session.goal,
    latestRequestId: session.latestRequestId,
    latestRequestOutcome: session.latestRequestOutcome,
  };
}
