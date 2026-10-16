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
import { AgentInitials } from "./parts";
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
import { useApp, useView } from "@/app/AppContext";
import {
  ActivityGroup,
  GroupStateContext,
  Reasoning,
  useGroupState,
} from "./ActivityGroup";

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

const ParentContext = createContext<ParentWork | null>(null);

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
          <UserInputWithState
            content={item.content ?? ""}
            state={state}
            actions={copyActions(item.content)}
          />
        );
      }
      const message = (
        <UserMessage actions={copyActions(item.content)}>{item.content}</UserMessage>
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

function ReplyText({ content, live }: { content: string; live: boolean }) {
  const [revealing] = useState(live);
  if (!revealing) return <Markdown>{content}</Markdown>;
  return <StreamText text={content} />;
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
      data-turn={
        item.kind === "userMessage" || item.kind === "pendingUserTurn" ? "" : undefined
      }
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
  scroller: HTMLElement | null;
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
  const continuing =
    Boolean(latest?.failureReason) && session?.turnState === "processing";
  const visible = useMemo(
    () =>
      (continuing
        ? session?.timelineItems.filter((item) => item.kind !== "liveAssistant")
        : session?.timelineItems) ?? [],
    [continuing, session?.timelineItems],
  );
  const showError =
    Boolean(responseError) && !wasInterrupted && (continuing || !inFlight);

  const sessionKey = session?.sessionId ?? null;
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
      className="mt-6 grid grid-cols-[minmax(0,1fr)] gap-5 [--step-scroll-inset:56]"
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
      <div className="relative min-w-0">
        <div data-slot="activity-line" className="prose-app px-2">
          <ActivityLine status={status} />
        </div>
      </div>
    </div>
  );
});
