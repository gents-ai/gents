import {
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import type { OptimisticPendingTurn } from "../../chat-shell.js";
import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { LIST_WINDOW_THRESHOLD } from "@source-inc/gents-desktop-ui";
import { isTerminalTurnState } from "@source-inc/gents-desktop-client";
import { MessageList } from "../Transcript.js";
import { coalesceToolGroups } from "../transcript/CompactToolGroup.js";

export type ChatTranscriptPanelProps = {
  selectedSessionId: string | null;
  session: DesktopSessionSnapshot | null;
  optimisticPendingTurn?: OptimisticPendingTurn | null;
  activity?: ReactNode;
  onEditUserMessage?: (
    item: Extract<RenderedTimelineItem, { kind: "userMessage" }>,
  ) => void;
  onRetryMessage?: (requestId: string) => void | Promise<void>;
  retryUnavailableHint?: string | null;
  onLoadOlder?: () => Promise<boolean>;
  showGoal?: boolean;
  compactTools?: boolean;
};

export const TRANSCRIPT_PAGE_SIZE = 40;
const TRANSCRIPT_RETAINED_ITEMS = TRANSCRIPT_PAGE_SIZE * 2;
const RESPONSE_ERROR_SUMMARY = "The assistant couldn't complete this turn.";

function timelineChangeSignal(items: RenderedTimelineItem[]) {
  return JSON.stringify(
    items.map((item) => {
      switch (item.kind) {
        case "assistantMessage":
        case "liveAssistant":
          return [
            item.kind,
            item.itemKey,
            item.content?.length ?? 0,
            item.reasoning?.length ?? 0,
          ];
        case "userMessage":
        case "automatedInput":
          return [item.kind, item.itemKey, item.content?.length ?? 0];
        case "pendingUserTurn":
          return [
            item.kind,
            item.itemKey,
            item.content.length,
            item.lifecycleState ?? "",
          ];
        case "requestContextReductionSummary":
          return [
            item.kind,
            item.itemKey,
            item.reductionCount,
            item.compactedCount,
            item.repairedCount,
            item.latestTimestamp ?? "",
          ];
        case "sessionCompaction":
          return [item.kind, item.itemKey, item.messagesCompacted ?? -1];
        case "toolGroup":
          return [
            item.kind,
            item.itemKey,
            item.tools.map((tool) => [
              tool.itemKey,
              tool.statusKind,
              tool.presentation,
              tool.partialOutputTail?.length ?? 0,
              tool.cancelCause?.cause ?? "",
            ]),
          ];
      }
    }),
  );
}

function scrollPanelToTip(panel: HTMLElement) {
  panel.scrollTo({ top: panel.scrollHeight, behavior: "instant" });
}

/** Viewport corrections must be immediate; a delayed correction can target a different row range. */
export function ChatTranscriptPanel({
  selectedSessionId,
  session,
  optimisticPendingTurn,
  activity,
  onEditUserMessage,
  onRetryMessage,
  retryUnavailableHint,
  onLoadOlder,
  showGoal = true,
  compactTools = false,
}: ChatTranscriptPanelProps) {
  const [transcriptPanel, setTranscriptPanel] = useState<HTMLElement | null>(
    null,
  );
  const [autoFollowTranscript, setAutoFollowTranscript] = useState(true);
  const [transcriptWindow, setTranscriptWindow] = useState({
    sessionId: selectedSessionId,
    timelineLength: 0,
    visibleCount: TRANSCRIPT_PAGE_SIZE,
  });
  const prependScrollHeightRef = useRef<number | null>(null);
  const pageFlight = useRef(false);
  const pageGeneration = useRef(0);
  const lastScrollTop = useRef(0);
  const touchY = useRef<number | undefined>(undefined);
  const [retryingRequestId, setRetryingRequestId] = useState<string | null>(
    null,
  );
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [awayFromBottom, setAwayFromBottom] = useState(false);
  const [newContentBelow, setNewContentBelow] = useState(false);

  const sourceTimelineItems = useMemo<RenderedTimelineItem[]>(() => {
    const durable = session?.timelineItems ?? [];
    if (
      !optimisticPendingTurn ||
      optimisticPendingTurn.sessionId !== selectedSessionId
    ) {
      return durable;
    }
    const hasOwner = durable.some(
      (item) =>
        (item.kind === "pendingUserTurn" &&
          item.requestId === optimisticPendingTurn.requestId) ||
        (item.kind === "userMessage" &&
          item.requestId === optimisticPendingTurn.requestId),
    );
    if (hasOwner) {
      return durable;
    }
    return [
      ...durable,
      {
        kind: "pendingUserTurn",
        itemKey: `optimistic-${optimisticPendingTurn.requestId}`,
        requestId: optimisticPendingTurn.requestId,
        content: optimisticPendingTurn.content,
        selectedSkillIds: optimisticPendingTurn.selectedSkillIds,
        capturedContext: optimisticPendingTurn.capturedContext,
        lifecycleState: optimisticPendingTurn.lifecycleState,
        foldedIntoRequestId: null,
        origin: null,
        createdAt: optimisticPendingTurn.createdAt,
      },
    ];
  }, [optimisticPendingTurn, selectedSessionId, session?.timelineItems]);

  const timelineItems = useMemo(
    () =>
      compactTools
        ? coalesceToolGroups(sourceTimelineItems)
        : sourceTimelineItems,
    [compactTools, sourceTimelineItems],
  );

  const baseVisibleCount =
    transcriptWindow.sessionId === selectedSessionId
      ? transcriptWindow.visibleCount
      : TRANSCRIPT_PAGE_SIZE;
  const visibleCount =
    !autoFollowTranscript && transcriptWindow.sessionId === selectedSessionId
      ? baseVisibleCount +
        Math.max(0, timelineItems.length - transcriptWindow.timelineLength)
      : baseVisibleCount;
  const firstVisibleIndex = Math.max(0, timelineItems.length - visibleCount);
  const visibleTimelineItems = useMemo(
    () => timelineItems.slice(firstVisibleIndex),
    [firstVisibleIndex, timelineItems],
  );
  const windowed =
    !!transcriptPanel && visibleTimelineItems.length > LIST_WINDOW_THRESHOLD;
  const hasLocalOlderItems = firstVisibleIndex > 0;
  const hasOlderItems =
    hasLocalOlderItems ||
    Boolean(session?.timelinePage?.hasOlder && onLoadOlder);
  const transcriptChange = useMemo(
    () => timelineChangeSignal(timelineItems),
    [timelineItems],
  );

  const tipChange = useMemo(
    () => timelineChangeSignal(sourceTimelineItems.slice(-1)),
    [sourceTimelineItems],
  );
  const observedTip = useRef(tipChange);
  useLayoutEffect(() => {
    if (autoFollowTranscript) setNewContentBelow(false);
    else if (observedTip.current !== tipChange) setNewContentBelow(true);
    observedTip.current = tipChange;
    if (transcriptPanel)
      setAwayFromBottom(
        transcriptPanel.scrollHeight -
          transcriptPanel.scrollTop -
          transcriptPanel.clientHeight >=
          64,
      );
  }, [tipChange, autoFollowTranscript, transcriptPanel, transcriptChange]);

  const lastItem = timelineItems[timelineItems.length - 1];
  const latestUserTurn = useMemo(() => {
    const items = timelineItems;
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const item = items[index];
      if (item.kind === "userMessage" || item.kind === "pendingUserTurn") {
        return item;
      }
    }
    return null;
  }, [timelineItems]);
  const optimisticSendIdentity =
    optimisticPendingTurn?.sessionId === selectedSessionId &&
    timelineItems.some(
      (item) =>
        item.kind === "pendingUserTurn" &&
        item.requestId === optimisticPendingTurn.requestId,
    )
      ? `request:${optimisticPendingTurn.requestId}`
      : null;
  const sendIdentity = optimisticSendIdentity
    ? optimisticSendIdentity
    : latestUserTurn?.requestId
      ? `request:${latestUserTurn.requestId}`
      : latestUserTurn
        ? `message:${latestUserTurn.itemKey}`
        : null;
  useLayoutEffect(() => {
    setTranscriptWindow({
      sessionId: selectedSessionId,
      timelineLength: timelineItems.length,
      visibleCount: TRANSCRIPT_PAGE_SIZE,
    });
    prependScrollHeightRef.current = null;
    pageGeneration.current += 1;
    pageFlight.current = false;
    setLoadingOlder(false);
    lastScrollTop.current = 0;
    setAutoFollowTranscript(true);
    const panel = transcriptPanel;
    if (panel) scrollPanelToTip(panel);
  }, [selectedSessionId, transcriptPanel]);

  useLayoutEffect(() => {
    if (!sendIdentity) {
      return;
    }
    setAutoFollowTranscript(true);
    const panel = transcriptPanel;
    if (panel) scrollPanelToTip(panel);
  }, [sendIdentity, transcriptPanel]);

  useLayoutEffect(() => {
    if (!transcriptPanel || typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(() => {
      if (autoFollowTranscript) scrollPanelToTip(transcriptPanel);
      setAwayFromBottom(
        transcriptPanel.scrollHeight -
          transcriptPanel.scrollTop -
          transcriptPanel.clientHeight >=
          64,
      );
    });
    observer.observe(transcriptPanel);
    return () => observer.disconnect();
  }, [transcriptPanel, autoFollowTranscript]);

  useLayoutEffect(() => {
    if (!autoFollowTranscript) {
      return;
    }

    setTranscriptWindow((current) =>
      current.sessionId === selectedSessionId &&
      current.timelineLength === timelineItems.length
        ? current
        : {
            sessionId: selectedSessionId,
            timelineLength: timelineItems.length,
            visibleCount:
              current.sessionId === selectedSessionId
                ? current.visibleCount
                : TRANSCRIPT_PAGE_SIZE,
          },
    );
    const panel = transcriptPanel;
    if (panel) scrollPanelToTip(panel);
  }, [
    autoFollowTranscript,
    transcriptPanel,
    selectedSessionId,
    transcriptChange,
    timelineItems.length,
    session?.turnState,
    activity,
  ]);

  useLayoutEffect(() => {
    const previousScrollHeight = prependScrollHeightRef.current;
    const panel = transcriptPanel;
    if (previousScrollHeight == null || !panel || pageFlight.current) return;

    prependScrollHeightRef.current = null;
    if (!windowed) panel.scrollTop += panel.scrollHeight - previousScrollHeight;
    lastScrollTop.current = panel.scrollTop;
  }, [
    firstVisibleIndex,
    loadingOlder,
    timelineItems.length,
    session?.timelinePage?.oldestItemKey,
    transcriptPanel,
    windowed,
  ]);

  async function loadOlderItems() {
    const panel = transcriptPanel;
    if (
      !panel ||
      !hasOlderItems ||
      pageFlight.current ||
      prependScrollHeightRef.current != null
    ) {
      return;
    }

    const generation = pageGeneration.current;
    prependScrollHeightRef.current = panel.scrollHeight;
    setAutoFollowTranscript(false);
    setTranscriptWindow({
      sessionId: selectedSessionId,
      timelineLength: timelineItems.length,
      visibleCount:
        visibleCount + (hasLocalOlderItems ? TRANSCRIPT_PAGE_SIZE : 0),
    });
    if (!hasLocalOlderItems && onLoadOlder) {
      pageFlight.current = true;
      setLoadingOlder(true);
      try {
        const loaded = await onLoadOlder();
        if (generation === pageGeneration.current && !loaded)
          prependScrollHeightRef.current = null;
      } catch {
        if (generation === pageGeneration.current)
          prependScrollHeightRef.current = null;
      } finally {
        if (generation === pageGeneration.current) {
          pageFlight.current = false;
          setLoadingOlder(false);
        }
      }
    }
  }

  async function handleRetry(requestId: string) {
    if (!onRetryMessage || retryingRequestId) return;
    setRetryingRequestId(requestId);
    try {
      await onRetryMessage(requestId);
    } finally {
      setRetryingRequestId(null);
    }
  }

  const requestOutcome = session?.latestRequestOutcome;
  const responseError = requestOutcome?.failureReason?.trim() ?? "";
  const responseWasCancelled = Boolean(requestOutcome?.cancelCause);
  const showResponseError = Boolean(responseError) && !responseWasCancelled;
  const retryRequestId = session?.latestRequestId ?? null;
  const retryEligible = session?.retryEligibility?.eligible ?? false;

  function handleTranscriptScroll() {
    const panel = transcriptPanel;
    if (!panel) {
      return;
    }

    const upward = panel.scrollTop < lastScrollTop.current;
    lastScrollTop.current = panel.scrollTop;
    const remaining = panel.scrollHeight - panel.scrollTop - panel.clientHeight;
    const atTip = remaining < 64;
    setAutoFollowTranscript(atTip);
    setAwayFromBottom(!atTip);
    setTranscriptWindow((current) => {
      const currentVisibleCount =
        current.sessionId === selectedSessionId
          ? current.visibleCount +
            Math.max(0, timelineItems.length - current.timelineLength)
          : TRANSCRIPT_PAGE_SIZE;
      const nextVisibleCount = atTip
        ? Math.min(currentVisibleCount, TRANSCRIPT_RETAINED_ITEMS)
        : currentVisibleCount;
      if (
        current.sessionId === selectedSessionId &&
        current.timelineLength === timelineItems.length &&
        current.visibleCount === nextVisibleCount
      ) {
        return current;
      }
      return {
        sessionId: selectedSessionId,
        timelineLength: timelineItems.length,
        visibleCount: nextVisibleCount,
      };
    });
    if (upward && panel.scrollTop <= 160) void loadOlderItems();
  }

  const turnActive = Boolean(
    session?.turnState && !isTerminalTurnState(session.turnState),
  );
  const assistantSilent =
    !lastItem ||
    lastItem.kind === "userMessage" ||
    lastItem.kind === "pendingUserTurn" ||
    (lastItem.kind === "liveAssistant" &&
      !(lastItem.content?.length || lastItem.reasoning?.length));
  const showThinking = turnActive && assistantSilent && !showResponseError;

  return (
    <section
      className="panel transcript-panel"
      data-scroll-owner="transcript"
      data-testid="transcript-panel"
      aria-label="Conversation"
      tabIndex={0}
      onScroll={handleTranscriptScroll}
      onWheel={(event) => {
        if (event.deltaY < 0 && event.currentTarget.scrollTop <= 160)
          void loadOlderItems();
      }}
      onTouchStart={(event) => {
        touchY.current = event.touches[0]?.clientY;
      }}
      onTouchMove={(event) => {
        const nextY = event.touches[0]?.clientY;
        if (
          touchY.current !== undefined &&
          nextY !== undefined &&
          nextY > touchY.current &&
          event.currentTarget.scrollTop <= 160
        )
          void loadOlderItems();
        touchY.current = nextY;
      }}
      onKeyDown={(event) => {
        if (
          ["ArrowUp", "PageUp", "Home"].includes(event.key) &&
          event.currentTarget.scrollTop <= 160
        )
          void loadOlderItems();
      }}
      ref={setTranscriptPanel}
    >
      {selectedSessionId && (session || timelineItems.length > 0) ? (
        <div className="message-list">
          {showGoal && session?.goal ? (
            <article className="message-card" data-testid="durable-goal-card">
              <div className="message-role">
                durable goal · {session.goal.status ?? "unknown"}
              </div>
              <div className="message-content">
                {session.goal.objective ?? "No objective"}
              </div>
              <div className="muted">
                {session.goal.tokensUsed}
                {session.goal.tokenBudget != null
                  ? ` / ${session.goal.tokenBudget}`
                  : ""}{" "}
                charged tokens · {session.goal.activeTimeSeconds}s active
              </div>
            </article>
          ) : null}
          {hasOlderItems ? (
            <div
              className="transcript-history-control"
              data-testid="transcript-older-status"
            >
              <button
                className="transcript-load-older"
                type="button"
                aria-label="Load older messages"
                disabled={loadingOlder}
                onClick={() => void loadOlderItems()}
              >
                {loadingOlder ? "Loading history…" : "Load older messages"}
              </button>
              <span
                className="transcript-history-announcement"
                role="status"
                aria-live="polite"
              >
                {loadingOlder ? "Loading older messages" : ""}
              </span>
            </div>
          ) : null}
          <MessageList
            onEditUserMessage={onEditUserMessage}
            compactTools={compactTools}
            timelineItems={visibleTimelineItems}
            timelineIdentity={selectedSessionId}
            scrollElement={transcriptPanel}
            requestCancelCause={requestOutcome?.cancelCause}
          />
          {activity === undefined && showThinking ? (
            <div className="turn-block">
              <article
                className="message-card thinking-card"
                data-testid="assistant-thinking"
                role="status"
                aria-label="Assistant is working"
              >
                <div className="message-role">assistant</div>
                <div className="assistant-activity">
                  <div className="thinking-dots" aria-hidden="true">
                    <span />
                    <span />
                    <span />
                  </div>
                  <div className="assistant-activity-copy">
                    <strong>
                      {session?.turnState === "waitingForClaim"
                        ? "Waiting for the node"
                        : "Assistant is working"}
                    </strong>
                    <span>
                      {session?.turnState === "waitingForClaim"
                        ? "Your message is queued and ready to be claimed."
                        : "Progress will appear here as it arrives."}
                    </span>
                  </div>
                </div>
              </article>
            </div>
          ) : null}
          {showResponseError ? (
            <div className="turn-block">
              <article
                className="message-card response-error-card"
                data-testid="response-error-card"
                role="alert"
              >
                <div className="message-role">assistant error</div>
                <div className="message-content">{RESPONSE_ERROR_SUMMARY}</div>
                <details className="response-error-details">
                  <summary>Error details</summary>
                  <pre className="response-error-content">{responseError}</pre>
                </details>
                {onRetryMessage && retryRequestId && retryEligible ? (
                  <div>
                    <button
                      className="ghost-button"
                      data-testid="retry-turn"
                      type="button"
                      disabled={
                        retryingRequestId === retryRequestId ||
                        Boolean(retryUnavailableHint)
                      }
                      title={retryUnavailableHint ?? undefined}
                      onClick={() => void handleRetry(retryRequestId)}
                    >
                      {retryingRequestId === retryRequestId
                        ? "Retrying…"
                        : "Retry"}
                    </button>
                  </div>
                ) : null}
              </article>
            </div>
          ) : null}
          {activity}
          <div className="transcript-end-anchor" />
        </div>
      ) : selectedSessionId ? (
        <div
          className="transcript-loading"
          data-testid="transcript-loading"
          role="status"
          aria-label="Loading conversation"
        >
          <div className="skeleton-row" />
          <div className="skeleton-row" />
          <div className="skeleton-row" />
        </div>
      ) : (
        <div className="empty-transcript compact-empty">
          <p className="eyebrow">Start Here</p>
          <h3>Send the first message</h3>
          <p className="muted">
            The first message creates the conversation automatically.
          </p>
        </div>
      )}
      {!autoFollowTranscript && awayFromBottom ? (
        <div className="transcript-jump">
          <button
            type="button"
            className="transcript-jump-button"
            aria-label="Scroll to bottom"
            title={
              newContentBelow
                ? "New activity below. Scroll to bottom"
                : "Scroll to bottom"
            }
            onClick={() => {
              setAutoFollowTranscript(true);
              setNewContentBelow(false);
              if (transcriptPanel) {
                transcriptPanel.focus({ preventScroll: true });
                scrollPanelToTip(transcriptPanel);
              }
            }}
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 16 16"
              fill="none"
              aria-hidden="true"
            >
              {newContentBelow ? (
                <g fill="currentColor">
                  <circle cx="3" cy="8" r="1" />
                  <circle cx="8" cy="8" r="1" />
                  <circle cx="13" cy="8" r="1" />
                </g>
              ) : (
                <path
                  d="M8 2.5v11m-4-4 4 4 4-4"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              )}
            </svg>
          </button>
        </div>
      ) : null}
    </section>
  );
}
