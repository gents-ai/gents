import { SentAttachments } from "./session/attachments";
import {
  Fragment,
  createContext,
  memo,
  useCallback,
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
import { useTranscriptList, type ReaderPlace } from "@/lib/transcriptList";
import { useHeldRows } from "@/lib/useHeldRows";
import { sequenceOf } from "@/lib/transcriptRows";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { Hint } from "./Hint";

import { ActivityLine } from "./Thinking";
import { activityStatus } from "./activity-status";
import { AgentInitials } from "./parts";
import { Markdown } from "./Markdown";
import { StreamText } from "./StreamText";
import {
  drawKey,
  drawKeys,
  holdLive,
  lingerSentTurn,
  noDrawKeys,
  withSentTurns,
  type DrawKeys,
  type HeldLive,
  type LocalTurn,
} from "./stream-reveal";
import { type Workers } from "./workers";
import {
  AutomatedInput,
  UserInputWithState,
  pendingInputState,
} from "./TranscriptInputs";
import { type ParentWork } from "./parentWork";
import { WorkerActionsContext, type WorkerActions } from "./WorkerActions";
import { readableReasoning, reasoningWithheld } from "./tool-summary";
import { groupTranscript, type TranscriptEntry } from "./transcript-groups";
import { useCopied } from "@/lib/clipboard";
import { toastFailure } from "@/lib/failure";
import { sessionViews } from "@/app/sessionViews";
import { createHeightLearner, guessHeight, type RowShape } from "@/lib/rowHeights";
import { useApp, useView } from "@/app/AppContext";
import {
  ActivityGroup,
  GroupStateContext,
  Reasoning,
  useGroupState,
} from "./ActivityGroup";

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

const ParentContext = createContext<ParentWork | null>(null);

function ModelInputReduction({
  item,
}: {
  item: Extract<RenderedTimelineItem, { kind: "requestContextReductionSummary" }>;
}) {
  return (
    <div
      className="mx-auto my-2 w-full max-w-3xl rounded-md border border-border/60 bg-muted/30 px-3 py-2 text-xs text-muted-foreground"
      data-testid="request-context-reduction-summary"
      data-request-doc-id={item.requestDocId}
    >
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
        <span className="font-medium text-foreground">Request context reductions</span>
        {item.latestTimestamp && (
          <time dateTime={item.latestTimestamp}>{item.latestTimestamp}</time>
        )}
      </div>
      <p className="mt-1">
        {item.reductionCount.toLocaleString()} affected model inputs
        {` · ${item.compactedCount.toLocaleString()} compacted`}
        {` · ${item.repairedCount.toLocaleString()} repaired`}
        {item.latestEstimatedInputTokens == null
          ? " · latest estimate unavailable"
          : ` · latest estimate ${item.latestEstimatedInputTokens.toLocaleString()} tokens`}
      </p>
    </div>
  );
}

function SessionCompaction({
  item,
}: {
  item: Extract<RenderedTimelineItem, { kind: "sessionCompaction" }>;
}) {
  return (
    <div
      className="mx-auto my-2 w-full max-w-3xl rounded-md border border-border/60 bg-muted/30 px-3 py-2 text-xs text-muted-foreground"
      data-testid="session-compaction-event"
      data-compaction-key={item.itemKey}
    >
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
        <span className="font-medium text-foreground">Session history compacted</span>
        {item.timestamp && <time dateTime={item.timestamp}>{item.timestamp}</time>}
      </div>
      <p className="mt-1">
        {item.messagesCompacted == null
          ? "Message count unavailable"
          : `${item.messagesCompacted.toLocaleString()} messages compacted`}
        {item.originalTokens == null || item.compactedTokens == null
          ? " · Token estimates unavailable"
          : ` · ${item.originalTokens.toLocaleString()} → ${item.compactedTokens.toLocaleString()} tokens`}
      </p>
    </div>
  );
}

const TranscriptItem = memo(function TranscriptItem({
  item,
  final = false,
}: {
  item: RenderedTimelineItem;
  final?: boolean;
}) {
  const parentWork = useContext(ParentContext);
  switch (item.kind) {
    case "automatedInput":
      return (
        <AutomatedInput
          origin={item.origin}
          content={item.content}
          reconstruction={item.reconstruction}
        />
      );
    case "userMessage":
    case "pendingUserTurn": {
      const state =
        item.kind === "pendingUserTurn"
          ? pendingInputState(item.lifecycleState, item.foldedIntoRequestId)
          : null;
      if (item.kind === "pendingUserTurn" && item.origin) {
        return (
          <AutomatedInput origin={item.origin} content={item.content} state={state} />
        );
      }
      const sender = parentWork?.sentBy(item.requestId) ?? null;
      if (!sender) {
        return (
          <div>
            <UserInputWithState
              content={item.content ?? ""}
              state={state}
              actions={copyActions(item.content)}
            />
            <SentAttachments
              items={item.attachments}
              requestId={item.requestId}
              messageKey={item.kind === "userMessage" ? item.itemKey : undefined}
            />
          </div>
        );
      }
      const message = (
        <div>
          <UserMessage actions={copyActions(item.content)}>{item.content}</UserMessage>
          <SentAttachments
            items={item.attachments}
            requestId={item.requestId}
            messageKey={item.kind === "userMessage" ? item.itemKey : undefined}
          />
        </div>
      );
      const senderName = sender.summary?.title ?? "another session";
      return (
        <div className={cn("relative", state && "mb-2")}>
          <AgentInitials
            name={sender.agentName ?? senderName}
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
      return null;
    case "requestContextReductionSummary":
      return <ModelInputReduction item={item} />;
    case "sessionCompaction":
      return <SessionCompaction item={item} />;
  }
});

const STOP_SOURCES: Record<string, string> = {
  requestInterrupt: "a stop request on this request",
  requestLifecycle: "the request's lifecycle state",
  deadline: "its deadline",
};

function StoppedNotice({
  cause,
  stoppedBeforeRunning,
}: {
  cause: DerivedCancelCauseView | null;
  stoppedBeforeRunning: boolean;
}) {
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
        <span>{stoppedBeforeRunning ? "Stopped before running" : "Stopped"}</span>
        <span>{headline}</span>
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
        className={item.content ? "py-4" : undefined}
        actions={!live && item.content && !final ? copyActions(item.content) : false}
      >
        {readableReasoning(item.reasoning) ? (
          <Reasoning
            text={readableReasoning(item.reasoning)!}
            stateKey={item.itemKey}
          />
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

function ReplyText({ content, live }: { content: string; live: boolean }) {
  const [revealing] = useState(live);
  if (!revealing) return <Markdown directives>{content}</Markdown>;
  return <StreamText text={content} directives />;
}

function ResponseActions({ text }: { text: string }) {
  const { copied, copy } = useCopied();
  return (
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

const ItemRow = memo(function ItemRow({
  rowKey,
  item,
  final,
}: {
  /** the key the row is drawn under, shared by every row for the message */
  rowKey: string;
  item: RenderedTimelineItem;
  final: boolean;
}) {
  return (
    <div
      data-timeline-key={rowKey}
      data-turn={
        item.kind === "userMessage" || item.kind === "pendingUserTurn" ? "" : undefined
      }
      className="group/response min-w-0 pb-5"
    >
      <TranscriptItem item={item} final={final} />
    </div>
  );
});

const GroupRow = memo(function GroupRow({
  entry,
  workers,
}: {
  entry: Extract<TranscriptEntry, { kind: "group" }>;
  workers: Workers;
}) {
  return (
    <div className="min-w-0 pb-5">
      <ActivityGroup entry={entry} workers={workers} />
    </div>
  );
});

/* the app's copy of the message just sent, drawn while no row stands for
   it: the bridge's pending row can leave a commit before its saved
   message lands */
function useSentCopy(items: RenderedTimelineItem[]): LocalTurn | null {
  const sent = useView((view) => view.pendingTurn);
  const [last, setLast] = useState<LocalTurn | null>(null);
  if (sent && sent !== last) setLast(sent);
  const copy = useMemo(() => lingerSentTurn(items, sent, last), [items, sent, last]);
  const lingering = sent === null && copy !== null;
  useEffect(() => {
    if (!lingering) return;
    const timer = setTimeout(() => setLast(null), LINGER_MS);
    return () => clearTimeout(timer);
  }, [lingering]);
  return copy;
}
const LINGER_MS = 3_000;

/* the scroll owner's face toward the composer: whether the foot is in view,
   the way back to it, and a nudge when the composer's room changes */
export type TranscriptOwner = {
  atBottom: boolean;
  toBottom: () => void;
  settle: () => void;
};

const EMPTY_ITEMS: RenderedTimelineItem[] = [];

/* what a row's height is guessed from before it is drawn */
function shapeOf(entry: TranscriptEntry): RowShape {
  if (entry.kind === "group")
    return { kind: "group", open: !entry.settled, steps: entry.members.length };
  const item = entry.item;
  switch (item.kind) {
    case "userMessage":
    case "pendingUserTurn":
    case "automatedInput":
      return {
        kind: "user",
        text: item.content ?? "",
        attachments: "attachments" in item ? item.attachments?.length : 0,
      };
    case "assistantMessage":
    case "liveAssistant":
      return { kind: "reply", text: item.content ?? "" };
    default:
      return { kind: "other" };
  }
}

export const TranscriptPanel = memo(function TranscriptPanel({
  inFlight,
  stopping = false,
  scroller,
  session,
  workers,
  parentWork,
  workerActions,
  sentTurn = null,
  anchorSentTurn = false,
  onOwner,
}: {
  inFlight: boolean;
  stopping?: boolean;
  scroller: HTMLElement | null;
  session: DesktopSessionSnapshot | null;
  workers: Workers;
  parentWork: ParentWork;
  workerActions: WorkerActions;
  sentTurn?: string | null;
  anchorSentTurn?: boolean;
  /* the session screen's composer reads the foot and the way back to it */
  onOwner?: (owner: TranscriptOwner) => void;
}) {
  const { actions } = useApp();
  const [retrying, setRetrying] = useState(false);
  const sessionKey = session?.sessionId ?? null;
  const rows = useHeldRows(
    sessionKey,
    session?.timelineItems ?? EMPTY_ITEMS,
    session?.timelinePage?.hasOlder ?? false,
    actions.loadSessionRows,
  );

  const latest = session?.latestRequestOutcome;
  const status = inFlight && rows.joined ? activityStatus(rows.items, stopping) : null;
  const wasInterrupted = session?.turnState === "interrupted";
  const responseError =
    latest?.failureReason?.trim() ||
    (session?.turnState === "failed"
      ? "The request failed before a response was available. Check the request trace for details."
      : "");
  const continuing =
    Boolean(latest?.failureReason) && session?.turnState === "processing";
  const visible = useMemo(
    () =>
      continuing
        ? rows.items.filter((item) => item.kind !== "liveAssistant")
        : rows.items,
    [continuing, rows.items],
  );
  const showError =
    Boolean(responseError) && !wasInterrupted && (continuing || !inFlight);

  const heldRef = useRef<HeldLive | null>(null);
  const [heldExpiry, setHeldExpiry] = useState(0);
  const held = useMemo(
    () =>
      continuing
        ? { items: visible, held: null }
        : holdLive(visible, heldRef.current, sessionKey),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [visible, continuing, heldExpiry, sessionKey],
  );
  heldRef.current = held.held;
  const holding =
    held.held !== null &&
    !visible.some((item) => item.kind === "liveAssistant" && Boolean(item.content));
  useEffect(() => {
    if (!holding) return;
    const timer = setTimeout(() => {
      heldRef.current = null;
      setHeldExpiry((n) => n + 1);
    }, 3_000);
    return () => clearTimeout(timer);
  }, [holding]);
  const localTurn = useSentCopy(held.items);
  const localQueued = useView(
    (view) =>
      view.sentTurns.find((sent) => sent.requestId === localTurn?.requestId)?.queued ??
      false,
  );
  const turns = useMemo(
    () =>
      rows.joined
        ? withSentTurns(held.items, localTurn, sessionKey, localQueued)
        : held.items,
    [rows.joined, held.items, localTurn, sessionKey, localQueued],
  );
  const keysRef = useRef<DrawKeys>(noDrawKeys(sessionKey));
  const keys = useMemo(
    () => drawKeys(keysRef.current, turns, held.replacedBy, sessionKey),
    [turns, held.replacedBy, sessionKey],
  );
  keysRef.current = keys;
  const entries = useMemo(() => groupTranscript(turns), [turns]);
  /* each drawn item's message number, by its item key: a group's key is
     the key of the item it opens with */
  const sequences = useMemo(
    () => new Map(turns.map((item) => [item.itemKey, sequenceOf(item)])),
    [turns],
  );
  const finalKeys = useMemo(() => {
    const keys = new Set<string>();
    let last: string | null = null;
    for (const e of entries) {
      if (e.kind === "group") {
        last = null;
        continue;
      }
      const k = e.item.kind;
      if (k === "userMessage" || k === "pendingUserTurn" || k === "automatedInput") {
        if (last) keys.add(last);
        last = null;
      } else if (k === "assistantMessage" && e.item.content?.trim()) last = e.key;
    }
    if (last && !inFlight) keys.add(last);
    return keys;
  }, [entries, inFlight]);
  const groupState = useGroupState(sessionKey);

  const entryKey = useCallback(
    (entry: TranscriptEntry) =>
      entry.kind === "item" ? drawKey(keys, entry.item) : entry.key,
    [keys],
  );
  const indexOf = useMemo(
    () => new Map(entries.map((entry, index) => [entryKey(entry), index])),
    [entries, entryKey],
  );
  const turnKeys = useMemo(
    () =>
      entries.flatMap((entry) =>
        entry.kind === "item" &&
        (entry.item.kind === "userMessage" || entry.item.kind === "pendingUserTurn")
          ? [entryKey(entry)]
          : [],
      ),
    [entries, entryKey],
  );
  /* a turn doing work: a tool step drawn after it */
  const turnWorks = useCallback(
    (key: string) => {
      const at = indexOf.get(key);
      return (
        at !== undefined &&
        entries.slice(at + 1).some((entry) => entry.kind === "group")
      );
    },
    [entries, indexOf],
  );

  /* opening a session returns to where the reader left it: at once when
     that row is drawn, else once the page around its message is read */
  const opening = useMemo(() => {
    const place = sessionViews.placeOf(sessionKey);
    if (!place) return null;
    if (indexOf.has(place.key)) return place;
    return place.sequence !== null ? ("awaiting" as const) : null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionKey, entries.length > 0]);

  /* the guesses, corrected by what this session's first rows measure */
  const heights = useMemo(() => createHeightLearner(), [sessionKey]);
  const estimateOf = useCallback(
    (entry: TranscriptEntry, width: number) => {
      const shape = shapeOf(entry);
      return Math.round(guessHeight(shape, width) * heights.scale(shape.kind));
    },
    [heights],
  );
  const onMeasured = useCallback(
    (entry: TranscriptEntry, height: number, width: number) => {
      const shape = shapeOf(entry);
      heights.learn(shape.kind, guessHeight(shape, width), height);
    },
    [heights],
  );

  const list = useTranscriptList({
    scroller,
    subject: sessionKey,
    entries,
    keyOf: entryKey,
    estimateOf,
    onMeasured,
    sentTurn: rows.joined ? sentTurn : null,
    anchorSentTurn,
    turnWorks,
    turns: turnKeys,
    opening,
    nearTop: () => rows.loadOlder(),
    nearFoot: () => rows.loadNewer(),
  });

  const returning = useRef<string | null>(null);
  useEffect(() => {
    if (!sessionKey || returning.current === sessionKey || entries.length === 0) return;
    returning.current = sessionKey;
    const place = sessionViews.placeOf(sessionKey);
    if (!place || indexOf.has(place.key) || place.sequence === null) return;
    rows.jump(place.sequence, () => {
      pendingPlace.current = place;
    });
  });
  const pendingPlace = useRef<ReaderPlace | null>(null);
  useEffect(() => {
    const place = pendingPlace.current;
    if (place && indexOf.has(place.key)) {
      pendingPlace.current = null;
      list.goTo(place);
    }
  });

  /* the reader's place, kept as they read; at the foot there is none */
  const { place } = list;
  useEffect(() => {
    if (!scroller || !sessionKey) return;
    let timer = 0;
    const remember = () => {
      if (timer) return;
      timer = window.setTimeout(() => {
        timer = 0;
        const at = place();
        const entry = at ? entries[indexOf.get(at.key) ?? -1] : undefined;
        sessionViews.setPlace(
          sessionKey,
          at ? { ...at, sequence: entrySequence(entry, sequences) } : null,
        );
      }, 300);
    };
    scroller.addEventListener("scroll", remember, { passive: true });
    return () => {
      scroller.removeEventListener("scroll", remember);
      window.clearTimeout(timer);
    };
  }, [scroller, sessionKey, place, entries, indexOf, sequences]);

  /* sending, or going back to the foot, returns to the newest page */
  const { atBottom, toBottom, settle } = list;
  const { joined, reset } = rows;
  const toNewest = useCallback(() => {
    if (!joined) reset();
    toBottom();
  }, [joined, reset, toBottom]);
  useEffect(() => {
    if (!joined && sentTurn) reset();
  }, [joined, sentTurn, reset]);
  useEffect(() => {
    onOwner?.({ atBottom: atBottom && joined, toBottom: toNewest, settle });
  }, [onOwner, atBottom, joined, toNewest, settle]);

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
    <div
      className="mt-6 grid grid-cols-[minmax(0,1fr)] [--step-scroll-inset:56]"
      data-testid="transcript-panel"
    >
      {rows.olderBefore !== null && (
        <div
          role="status"
          aria-live="polite"
          className="mb-5 min-h-5 text-center text-xs text-muted-foreground"
          data-testid="transcript-older-status"
        >
          {rows.loading === "older" ? "Loading older messages…" : null}
        </div>
      )}
      <WorkerActionsContext.Provider value={workerActions}>
        <ParentContext.Provider value={parentWork}>
          <GroupStateContext.Provider value={groupState}>
            <div {...list.listProps} data-transcript-list data-rows={entries.length}>
              {list.items.map(({ index, start }) => {
                const entry = entries[index]!;
                return (
                  <div
                    key={entryKey(entry)}
                    {...list.attributes(index, start)}
                    className="grid min-w-0 grid-cols-[minmax(0,1fr)]"
                  >
                    {entry.kind === "item" ? (
                      <ItemRow
                        rowKey={drawKey(keys, entry.item)}
                        item={entry.item}
                        final={finalKeys.has(entry.key)}
                      />
                    ) : (
                      <GroupRow entry={entry} workers={workers} />
                    )}
                  </div>
                );
              })}
            </div>
            {rows.joined ? (
              <div className="grid min-w-0 grid-cols-[minmax(0,1fr)] content-start gap-5">
                {continuing && showError && <FailedEarlier message={responseError} />}
                {continuing &&
                  session?.timelineItems
                    .filter((item) => item.kind === "liveAssistant")
                    .map((item) => <TranscriptItem key={item.itemKey} item={item} />)}
                {wasInterrupted && !inFlight && (
                  <StoppedNotice
                    cause={session?.latestRequestOutcome?.cancelCause ?? null}
                    stoppedBeforeRunning={
                      session?.latestRequestOutcome?.stoppedBeforeRunning === true
                    }
                  />
                )}
                {showError && (
                  <div className="rounded-2xl border border-destructive/30 bg-destructive/5 px-4 py-3">
                    <p className="text-sm font-medium">
                      The assistant could not finish this turn.
                    </p>
                    <pre className="mt-1 font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
                      {responseError}
                    </pre>
                    {session?.timelineItems.some(
                      (item) =>
                        (item.kind === "userMessage" ||
                          item.kind === "pendingUserTurn") &&
                        !!item.attachments?.length,
                    ) && (
                      <p className="mt-2 text-xs text-muted-foreground">
                        Attachments stay in this conversation. Start a new chat to
                        continue without them.
                      </p>
                    )}
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
                {/* the run's line: laid out as a reply is, but not one */}
                <div className="relative min-w-0">
                  <div data-slot="activity-line" className="prose-app px-2">
                    <ActivityLine status={status} />
                  </div>
                </div>
              </div>
            ) : (
              <div
                className="min-h-5 text-center text-xs text-muted-foreground"
                data-testid="transcript-newer-status"
              >
                {rows.loading === "newer" ? "Loading newer messages…" : null}
              </div>
            )}
            {/* the room a held turn's reply fills, below everything drawn */}
            <div aria-hidden="true" style={{ height: list.room }} />
          </GroupStateContext.Provider>
        </ParentContext.Provider>
      </WorkerActionsContext.Provider>
    </div>
  );
});

/* the message number an entry stands at */
function entrySequence(
  entry: TranscriptEntry | undefined,
  sequences: Map<string, number | null>,
): number | null {
  if (!entry) return null;
  return entry.kind === "item"
    ? sequenceOf(entry.item)
    : (sequences.get(entry.key) ?? null);
}
