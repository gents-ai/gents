/* The session's transcript: its rows, the live reply, notices and the
   actions on a response. */
import {
  Fragment,
  createContext,
  memo,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Check, Copy } from "lucide-react";
import { toast } from "sonner";
import type {
  DerivedCancelCauseView,
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { AssistantMessage, UserMessage } from "@gents/ui/conversation";
import { useOlderPages } from "@/lib/scroll";
import { TranscriptWindowProvider, WindowedRow } from "@/lib/transcriptWindow";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { Hint } from "./Hint";

import { ActivityLine } from "./Thinking";
import { activityStatus } from "./activity-status";
import { BehaviorAvatar } from "./parts";
import { Markdown } from "./Markdown";
import { StreamText } from "./StreamText";
import {
  drawKey,
  drawKeys,
  holdLive,
  noDrawKeys,
  withSentTurns,
  type DrawKeys,
  type HeldLive,
} from "./stream-reveal";
import { type Workers } from "./workers";
import { type ParentWork } from "./parentWork";
import { WorkerActionsContext, type WorkerActions } from "./WorkerActions";
import { readableReasoning, reasoningWithheld } from "./tool-summary";
import { groupTranscript, type TranscriptEntry } from "./transcript-groups";
import { useCopied } from "@/lib/clipboard";
import { toastFailure } from "@/lib/failure";
import { useApp, useView } from "@/app/AppContext";
import {
  ActivityGroup,
  GroupStateContext,
  Reasoning,
  useGroupState,
} from "./ActivityGroup";
/* the newest entries are always drawn: the turn in progress and the few
   before it are where the reader is, and where the transcript grows */
const ALWAYS_DRAWN = 8;

function FailedEarlier({ message }: { message: string }) {
  return (
    <div className="rounded-2xl border border-destructive/30 bg-destructive/5 px-4 py-3">
      <p className="text-sm font-medium">
        This request could not finish. The session continued on a later one.
      </p>
      <pre className="mt-1 font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
        {message}
      </pre>
    </div>
  );
}

function copyActions(text: string | null | undefined) {
  return [
    {
      label: "Copy",
      icon: <Copy />,
      onClick: () => {
        void navigator.clipboard?.writeText(text ?? "");
        toast("Copied");
      },
    },
  ];
}

/* the sessions that sent work into this one; a turn another session sent is
   labeled with its sender */
const ParentContext = createContext<ParentWork | null>(null);

const TranscriptItem = memo(function TranscriptItem({
  item,
  final = false,
}: {
  item: RenderedTimelineItem;
  /* the answer a finished turn ended on: it carries the response actions */
  final?: boolean;
}) {
  const parentWork = useContext(ParentContext);
  switch (item.kind) {
    case "userMessage":
    case "pendingUserTurn": {
      const sender = parentWork?.sentBy(item.requestId) ?? null;
      const message = (
        <UserMessage actions={copyActions(item.content)}>{item.content}</UserMessage>
      );
      if (!sender) return message;
      const senderName = sender.summary?.title ?? "another session";
      /* a turn another session sent wears that session's mark, the way any
         other sender would; its state, where the mark cannot say it, is a
         chip seated on the bubble's bottom edge */
      const state = item.kind === "pendingUserTurn" ? "Queued" : null;
      return (
        /* the mark hangs in the transcript's right gutter, seated on the
           first line's center: the bubble's own my-1 and py-3 put that 26px
           down, half the avatar is 12 */
        <div className={cn("relative", state && "mb-2")}>
          <BehaviorAvatar
            name={sender.behaviorName ?? senderName}
            /* in the gutter where there is one; seated on the bubble's
               top corner when the screen is too narrow to spare it */
            className="absolute -top-1 right-2 size-6 text-[10px] ring-2 ring-background sm:top-3.5 sm:-right-8 sm:ring-0"
            aria-hidden={false}
            role="img"
            aria-label={`Sent by ${senderName}`}
            title={`Sent by ${senderName}`}
          />
          <div className="relative min-w-0">
            {message}
            {state && (
              <Badge
                variant="outline"
                className="absolute right-4 bottom-0 translate-y-1/2 border-border/60 bg-background text-[10px] font-normal text-muted-foreground"
              >
                {state}
              </Badge>
            )}
          </div>
        </div>
      );
    }
    case "assistantMessage":
    case "liveAssistant":
      return <Reply item={item} final={final} />;
    case "toolGroup":
      /* placed by the transcript into an ActivityGroup; never reaches here */
      return null;
  }
});

const STOP_SOURCES: Record<string, string> = {
  requestInterrupt: "a stop request on this request",
  requestLifecycle: "the request's lifecycle state",
  deadline: "its deadline",
};

function StoppedNotice({ cause }: { cause: DerivedCancelCauseView | null }) {
  const headline =
    cause?.cause === "userCancelled"
      ? "You stopped this response."
      : cause?.cause === "deadline"
        ? "This response stopped at its deadline."
        : "This response was stopped.";
  const at = cause?.at ? new Date(cause.at) : null;
  return (
    <Collapsible
      className="px-2 text-xs text-muted-foreground"
      data-testid="stopped-notice"
    >
      <p className="flex items-center gap-2">
        {headline}
        {cause && (
          <CollapsibleTrigger className="cursor-pointer underline decoration-border underline-offset-4 hover:text-foreground">
            Details
          </CollapsibleTrigger>
        )}
      </p>
      {cause && (
        <CollapsibleContent>
          <dl className="mt-2 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-3 gap-y-1 font-mono text-[11px]">
            <dt>stopped by</dt>
            <dd>{STOP_SOURCES[cause.source] ?? cause.source}</dd>
            {at && !Number.isNaN(at.getTime()) && (
              <>
                <dt>at</dt>
                <dd>{at.toLocaleTimeString()}</dd>
              </>
            )}
            {cause.evidence.map((line) => (
              <Fragment key={line}>
                <dt>evidence</dt>
                <dd className="break-all">{line}</dd>
              </Fragment>
            ))}
          </dl>
        </CollapsibleContent>
      )}
    </Collapsible>
  );
}

/* A reply, live or saved, drawn as one tree. The saved message that
   replaces a live tail is drawn under the tail's key (`drawKey`), so this
   same tree updates in place: nothing is removed and inserted under the
   reader, and the text carries on revealing. A turn that thought and then
   acted leaves reasoning with nothing said after it: that is a think, not
   an empty answer, so it carries no action bar and no blank line where
   prose would be. */
function Reply({
  item,
  final,
}: {
  item: Extract<RenderedTimelineItem, { kind: "assistantMessage" | "liveAssistant" }>;
  final: boolean;
}) {
  const live = item.kind === "liveAssistant";
  return (
    <div data-testid={live ? "live-assistant" : undefined}>
      <AssistantMessage
        /* the answer sits wider apart than the rows around it: the column's
           gap is even, which puts the thing a person came to read at the
           same distance as a row of activity */
        className={item.content ? "py-4" : undefined}
        actions={!live && item.content && !final ? copyActions(item.content) : false}
      >
        {/* a provider that will not hand its reasoning over leaves a
            placeholder, not prose: say so in a line rather than offer a
            disclosure with an apology behind it */}
        {readableReasoning(item.reasoning) ? (
          <Reasoning text={readableReasoning(item.reasoning)!} />
        ) : reasoningWithheld(item.reasoning) ? (
          <p className="pb-2 text-xs text-muted-foreground">
            Reasoning was not shared by the provider.
          </p>
        ) : null}
        {item.content ? <ReplyText content={item.content} live={live} /> : null}
      </AssistantMessage>
      {final && item.content ? <ResponseActions text={item.content} /> : null}
    </div>
  );
}

/* A reply drawn while it was live reveals at a steady pace, on through the
   message that replaces it; anything else — history, a message from
   elsewhere — is simply there. */
function ReplyText({ content, live }: { content: string; live: boolean }) {
  const [revealing] = useState(live);
  /* No wrapper: prose-app spaces its blocks with `& > * + *`, so anything
     between it and the markdown makes every paragraph, heading and list
     lose its margins at once — silently, because the text still renders. */
  if (!revealing) return <Markdown>{content}</Markdown>;
  return <StreamText text={content} />;
}

/* Under a finished response the actions sit on the line below the answer
   and stay there, rather than floating over it on hover. Copy takes the response as written — its markdown — and says so. */
function ResponseActions({ text }: { text: string }) {
  const { copied, copy } = useCopied();
  return (
    /* on a desktop they wait for the pointer, like a step's caret: an
       answer reads as the end of the turn, not as a row of controls. The
       space is kept, so nothing moves when they appear; a touch screen has
       no hover to wait for, and keyboard focus shows them too */
    <div
      className={cn(
        "-mt-1 flex items-center gap-1 px-1 transition-opacity duration-150 motion-reduce:transition-none",
        !copied &&
          "sm:opacity-0 sm:group-hover/response:opacity-100 sm:focus-within:opacity-100",
      )}
      data-testid="response-actions"
    >
      <Hint label={copied ? "Copied" : "Copy response"}>
        <Button
          variant="quiet"
          size="icon-xs"
          aria-label="Copy response"
          onClick={() => copy(text)}
        >
          {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
        </Button>
      </Hint>
    </div>
  );
}

/* A row in the transcript's window. Memoized on what it draws, so an update
   that leaves a row alone (every streamed chunk, for all but the last)
   renders neither the row nor its wrapper. */
const ItemRow = memo(function ItemRow({
  rowKey,
  timelineKey,
  item,
  final,
  drawn,
}: {
  rowKey: string;
  timelineKey: string;
  item: RenderedTimelineItem;
  final: boolean;
  drawn: boolean;
}) {
  return (
    <WindowedRow
      rowKey={rowKey}
      drawn={drawn}
      data-timeline-key={timelineKey}
      className="group/response"
    >
      <TranscriptItem item={item} final={final} />
    </WindowedRow>
  );
});

const GroupRow = memo(function GroupRow({
  entry,
  workers,
  drawn,
}: {
  entry: Extract<TranscriptEntry, { kind: "group" }>;
  workers: Workers;
  drawn: boolean;
}) {
  return (
    <WindowedRow rowKey={entry.key} drawn={drawn}>
      <ActivityGroup entry={entry} workers={workers} />
    </WindowedRow>
  );
});

export const TranscriptPanel = memo(function TranscriptPanel({
  inFlight,
  stopping = false,
  scroller,
  content,
  session,
  workers,
  parentWork,
  workerActions,
}: {
  inFlight: boolean;
  stopping?: boolean;
  /** the transcript's scroller, once mounted */
  scroller: HTMLElement | null;
  /** everything the scroller scrolls, once mounted */
  content: HTMLElement | null;
  session: DesktopSessionSnapshot | null;
  workers: Workers;
  parentWork: ParentWork;
  workerActions: WorkerActions;
}) {
  const { actions } = useApp();
  const loadingOlder = useOlderPages(
    scroller,
    content,
    session?.sessionId ?? null,
    session?.timelinePage?.hasOlder ?? false,
    () => actions.loadOlderSessionTimeline(),
    session?.timelineItems[0]?.itemKey ?? null,
  );
  const [retrying, setRetrying] = useState(false);

  /* the latest request's terminal facts; the bridge sends no response row */
  const latest = session?.latestRequestOutcome;
  const status = inFlight
    ? activityStatus(session?.timelineItems ?? [], stopping)
    : null;
  const wasInterrupted = session?.turnState === "interrupted";
  const responseError =
    latest?.failureReason?.trim() ||
    (session?.turnState === "failed"
      ? "The request failed before a response was available. Check the request trace for details."
      : "");
  /* the request that ended failed, and the session is already on a later
     one: both facts, in order, the failure before the live text */
  const continuing =
    Boolean(latest?.failureReason) && session?.turnState === "processing";
  /* the list the transcript actually maps, held stable so the fold's
     memo has a key that does not change on every render */
  const visible = useMemo(
    () =>
      (continuing
        ? session?.timelineItems.filter((item) => item.kind !== "liveAssistant")
        : session?.timelineItems) ?? [],
    [continuing, session?.timelineItems],
  );
  const showError =
    Boolean(responseError) && !wasInterrupted && (continuing || !inFlight);

  /* Streaming continuity: the live text is kept on screen until the
     message that replaces it arrives (a projection can drop one before the
     other lands), and that message is drawn under the live tail's key. */
  const sessionKey = session?.sessionId ?? null;
  const heldRef = useRef<HeldLive | null>(null);
  const [heldExpiry, setHeldExpiry] = useState(0);
  const held = useMemo(
    () =>
      continuing
        ? { items: visible, held: null }
        : holdLive(visible, heldRef.current, sessionKey),
    // heldExpiry: a hold that ran out re-derives without it
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [visible, continuing, heldExpiry, sessionKey],
  );
  heldRef.current = held.held;
  const holding =
    held.held !== null &&
    !visible.some((item) => item.kind === "liveAssistant" && Boolean(item.content));
  useEffect(() => {
    if (!holding) return;
    /* nothing replaced it: an interrupted turn, or a message that never
       came. Stop holding rather than show stale text indefinitely. */
    const timer = setTimeout(() => {
      heldRef.current = null;
      setHeldExpiry((n) => n + 1);
    }, 3_000);
    return () => clearTimeout(timer);
  }, [holding]);
  /* the message just sent, drawn from the moment it is sent: one row per
     message, whatever stands for it as it settles */
  const localTurn = useView((view) => view.pendingTurn);
  const turns = useMemo(
    () => withSentTurns(held.items, localTurn, sessionKey),
    [held.items, localTurn, sessionKey],
  );
  const keysRef = useRef<DrawKeys>(noDrawKeys(sessionKey));
  const keys = useMemo(
    () => drawKeys(keysRef.current, turns, held.replacedBy, sessionKey),
    [turns, held.replacedBy, sessionKey],
  );
  keysRef.current = keys;
  const rendered = turns;
  const entries = useMemo(() => groupTranscript(rendered), [rendered]);
  /* A turn's answer is the last thing it said before the person spoke
     again, or before the transcript ends once nothing is running. Only a
     finished turn has one: while the agent works, its last words are
     narration that may yet be followed by more. */
  const finalKeys = useMemo(() => {
    const keys = new Set<string>();
    let last: string | null = null;
    for (const e of entries) {
      if (e.kind === "group") {
        last = null;
        continue;
      }
      const k = e.item.kind;
      if (k === "userMessage" || k === "pendingUserTurn") {
        if (last) keys.add(last);
        last = null;
      } else if (k === "assistantMessage" && e.item.content?.trim()) last = e.key;
    }
    if (last && !inFlight) keys.add(last);
    return keys;
  }, [entries, inFlight]);
  const groupState = useGroupState(sessionKey);

  const retry = async () => {
    const requestId = session?.latestRequestId;
    if (!requestId) return;
    setRetrying(true);
    try {
      await actions.retryMessage(requestId);
    } catch (error) {
      toastFailure("retry the message", error);
    } finally {
      setRetrying(false);
    }
  };

  return (
    /* the condensed header floats over the scroller, so a step opening
       near the top must stop below it, not under it (h-12 plus a little) */
    <div
      /* the composer is sticky over the foot of this scroller, so the
         transcript keeps its height clear: without it the last thing in a
         session sits underneath the composer, and anything that sticks to
         the bottom lands there too */
      className="mt-6 grid grid-cols-[minmax(0,1fr)] gap-5 pb-[var(--composer-h,0px)] [--step-scroll-inset:56]"
      data-testid="transcript-panel"
    >
      {session?.timelinePage?.hasOlder && (
        <div
          role="status"
          aria-live="polite"
          className="min-h-5 text-center text-xs text-muted-foreground"
          data-testid="transcript-older-status"
        >
          {loadingOlder ? "Loading older messages…" : null}
        </div>
      )}
      <WorkerActionsContext.Provider value={workerActions}>
        <ParentContext.Provider value={parentWork}>
          <GroupStateContext.Provider value={groupState}>
            <TranscriptWindowProvider scroller={scroller} session={sessionKey}>
              {entries.map((entry, index) =>
                entry.kind === "item" ? (
                  /* keyed for the pager, which holds the reader's place
                     by the row under their eye while older pages land */
                  <ItemRow
                    key={drawKey(keys, entry.item)}
                    rowKey={drawKey(keys, entry.item)}
                    timelineKey={entry.key}
                    item={entry.item}
                    final={finalKeys.has(entry.key)}
                    drawn={entries.length - index <= ALWAYS_DRAWN}
                  />
                ) : (
                  <GroupRow
                    key={entry.key}
                    entry={entry}
                    workers={workers}
                    drawn={entries.length - index <= ALWAYS_DRAWN}
                  />
                ),
              )}
            </TranscriptWindowProvider>
          </GroupStateContext.Provider>
          {continuing && showError && <FailedEarlier message={responseError} />}
          {continuing &&
            session?.timelineItems
              .filter((item) => item.kind === "liveAssistant")
              .map((item) => <TranscriptItem key={item.itemKey} item={item} />)}
        </ParentContext.Provider>
      </WorkerActionsContext.Provider>
      {wasInterrupted && !inFlight && (
        <StoppedNotice cause={session?.latestRequestOutcome?.cancelCause ?? null} />
      )}
      {showError && (
        <div className="rounded-2xl border border-destructive/30 bg-destructive/5 px-4 py-3">
          <p className="text-sm font-medium">
            The assistant could not finish this turn.
          </p>
          <pre className="mt-1 font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
            {responseError}
          </pre>
          {session?.retryEligibility?.eligible && (
            <Button
              size="sm"
              variant="outline"
              className="mt-3"
              disabled={retrying}
              onClick={retry}
            >
              {retrying ? "Retrying…" : "Retry"}
            </Button>
          )}
        </div>
      )}
      {/* the run's line, in the place kept for it at the foot: laid out as a
          reply is, but not one, so nothing reading the transcript counts it */}
      <div className="relative min-w-0">
        <div data-slot="activity-line" className="prose-app px-2">
          <ActivityLine status={status} />
        </div>
      </div>
    </div>
  );
});
