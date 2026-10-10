import { useLayoutEffect, useMemo, useRef, useState } from "react";

import type { OptimisticPendingTurn } from "../../chat-shell.js";
import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { isTerminalTurnState } from "@source-inc/gents-desktop-client";
import { MessageList } from "../Transcript.js";

export type ChatTranscriptPanelProps = {
  selectedSessionId: string | null;
  session: DesktopSessionSnapshot | null;
  optimisticPendingTurn?: OptimisticPendingTurn | null;
  onRetryMessage?: (requestId: string) => void | Promise<void>;
  retryUnavailableHint?: string | null;
  onLoadOlder?: () => Promise<boolean>;
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
          return [item.kind, item.itemKey, item.content?.length ?? 0];
        case "pendingUserTurn":
          return [
            item.kind,
            item.itemKey,
            item.content.length,
            item.lifecycleState ?? "",
          ];
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
  // Instant, not smooth: the panel's CSS smooth-scroll otherwise animates the
  // jump. A chunk landing mid-animation can leave the scroll short of the tip,
  // which the scroll handler then misreads as the user disengaging follow.
  panel.scrollTo({ top: panel.scrollHeight, behavior: "instant" });
}

export function ChatTranscriptPanel({
  selectedSessionId,
  session,
  optimisticPendingTurn,
  onRetryMessage,
  retryUnavailableHint,
  onLoadOlder,
}: ChatTranscriptPanelProps) {
  const transcriptPanelRef = useRef<HTMLElement | null>(null);
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

  const timelineItems = useMemo<RenderedTimelineItem[]>(() => {
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
        lifecycleState: optimisticPendingTurn.lifecycleState,
        foldedIntoRequestId: null,
        origin: null,
        createdAt: optimisticPendingTurn.createdAt,
      },
    ];
  }, [optimisticPendingTurn, selectedSessionId, session?.timelineItems]);

  const baseVisibleCount =
    transcriptWindow.sessionId === selectedSessionId
      ? transcriptWindow.visibleCount
      : TRANSCRIPT_PAGE_SIZE;
  // Keep the leading edge stable while the reader is away from the tip. New
  // streaming items should extend the mounted window, not evict the row they
  // are currently reading.
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
  const hasLocalOlderItems = firstVisibleIndex > 0;
  const hasOlderItems =
    hasLocalOlderItems ||
    Boolean(session?.timelinePage?.hasOlder && onLoadOlder);
  const transcriptChange = useMemo(
    () => timelineChangeSignal(timelineItems),
    [timelineItems],
  );

  const lastItem = timelineItems[timelineItems.length - 1];
  // A send may be observed as pending or already materialized. Prefer the
  // request identity so that pending -> materialized does not look like a
  // second send; fall back to the user row identity for partial snapshots.
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
    : session?.latestRequestId
      ? `request:${session.latestRequestId}`
      : latestUserTurn?.kind === "pendingUserTurn"
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
    const panel = transcriptPanelRef.current;
    if (panel) scrollPanelToTip(panel);
  }, [selectedSessionId]);

  useLayoutEffect(() => {
    if (!sendIdentity) {
      return;
    }
    setAutoFollowTranscript(true);
    const panel = transcriptPanelRef.current;
    if (panel) scrollPanelToTip(panel);
  }, [sendIdentity]);

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
    const panel = transcriptPanelRef.current;
    if (panel) scrollPanelToTip(panel);
  }, [
    autoFollowTranscript,
    selectedSessionId,
    transcriptChange,
    timelineItems.length,
    session?.turnState,
  ]);

  useLayoutEffect(() => {
    const previousScrollHeight = prependScrollHeightRef.current;
    const panel = transcriptPanelRef.current;
    if (previousScrollHeight == null || !panel) return;

    prependScrollHeightRef.current = null;
    panel.scrollTop += panel.scrollHeight - previousScrollHeight;
    lastScrollTop.current = panel.scrollTop;
  }, [firstVisibleIndex, timelineItems.length]);

  async function loadOlderItems() {
    const panel = transcriptPanelRef.current;
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
      visibleCount: visibleCount + TRANSCRIPT_PAGE_SIZE,
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
    const panel = transcriptPanelRef.current;
    if (!panel) {
      return;
    }

    const upward = panel.scrollTop < lastScrollTop.current;
    lastScrollTop.current = panel.scrollTop;
    const remaining = panel.scrollHeight - panel.scrollTop - panel.clientHeight;
    const atTip = remaining < 64;
    setAutoFollowTranscript(atTip);
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

  // Animated placeholder between send and the assistant's first visible
  // output — without it the transcript sits inert while the turn runs.
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
      ref={transcriptPanelRef}
    >
      {selectedSessionId && (session || timelineItems.length > 0) ? (
        <div className="message-list">
          {session?.goal ? (
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
              role="status"
              aria-live="polite"
              data-testid="transcript-older-status"
            >
              {loadingOlder ? "Loading older messages…" : null}
            </div>
          ) : null}
          <MessageList
            timelineItems={visibleTimelineItems}
            timelineIdentity={selectedSessionId}
            requestCancelCause={requestOutcome?.cancelCause}
          />
          {showThinking ? (
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
    </section>
  );
}
