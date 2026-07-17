import { useEffect, useMemo, useRef, useState } from "react";

import { isTerminalTurnState } from "../../lib/chat-shell";
import type { DesktopSessionSnapshot } from "../../lib/types";
import { MessageList } from "../Transcript";

export type ChatTranscriptPanelProps = {
  selectedSessionId: string | null;
  session: DesktopSessionSnapshot | null;
};

export function ChatTranscriptPanel({
  selectedSessionId,
  session,
}: ChatTranscriptPanelProps) {
  const transcriptPanelRef = useRef<HTMLElement | null>(null);
  const transcriptEndRef = useRef<HTMLDivElement | null>(null);
  const [autoFollowTranscript, setAutoFollowTranscript] = useState(true);

  const transcriptSignature = useMemo(
    () =>
      JSON.stringify({
        sessionId: selectedSessionId,
        timelineLength: session?.timelineItems.length ?? 0,
        timelineKinds: session?.timelineItems.map((item) => item.kind) ?? [],
        timelineContentLengths:
          session?.timelineItems.map((item) => {
            switch (item.kind) {
              case "assistantMessage":
              case "liveAssistant":
                return [item.content?.length ?? 0, item.reasoning?.length ?? 0];
              case "userMessage":
              case "pendingUserTurn":
                return item.content.length;
              case "toolGroup":
                return item.tools.map((tool) => [
                  tool.status?.length ?? 0,
                  tool.args?.rawText.length ?? 0,
                  tool.result?.rawText.length ?? 0,
                ]);
            }
          }) ?? [],
        turnState: session?.turnState ?? "",
        latestResponseStatus: session?.latestResponse?.status ?? "",
        latestResponseError: session?.latestResponse?.errorMessage ?? "",
      }),
    [
      selectedSessionId,
      session?.timelineItems,
      session?.turnState,
      session?.latestResponse?.status,
      session?.latestResponse?.errorMessage,
    ],
  );

  useEffect(() => {
    setAutoFollowTranscript(true);
  }, [selectedSessionId]);

  // The user's own send always re-engages following — matching every chat
  // app's behavior — even if they had scrolled up to read history.
  const lastItem = session?.timelineItems[session.timelineItems.length - 1];
  const pendingSendKey =
    lastItem?.kind === "pendingUserTurn"
      ? `${session?.timelineItems.length}:${lastItem.content}`
      : null;
  useEffect(() => {
    if (pendingSendKey) {
      setAutoFollowTranscript(true);
    }
  }, [pendingSendKey]);

  useEffect(() => {
    if (!autoFollowTranscript) {
      return;
    }

    const scrollTarget = transcriptEndRef.current;
    if (!scrollTarget) {
      return;
    }

    const frame = window.requestAnimationFrame(() => {
      // Instant, not smooth: the panel's CSS smooth-scroll animates
      // scrollIntoView, and a chunk landing mid-animation left the scroll
      // short of the bottom — which the scroll handler then misread as the
      // user scrolling away, silently disengaging follow.
      scrollTarget.scrollIntoView({ block: "end", behavior: "instant" });
    });

    return () => window.cancelAnimationFrame(frame);
  }, [autoFollowTranscript, transcriptSignature]);

  function handleTranscriptScroll() {
    const panel = transcriptPanelRef.current;
    if (!panel) {
      return;
    }

    const remaining = panel.scrollHeight - panel.scrollTop - panel.clientHeight;
    setAutoFollowTranscript(remaining < 64);
  }

  const responseError = session?.latestResponse?.errorMessage?.trim() ?? "";
  const showResponseError = Boolean(responseError);

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
      data-testid="transcript-panel"
      onScroll={handleTranscriptScroll}
      ref={transcriptPanelRef}
    >
      {selectedSessionId && session ? (
        <div className="message-list">
          {session.goal ? (
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
          <MessageList
            timelineItems={session.timelineItems}
            responseCancelCause={session.latestResponse?.cancelCause}
            responseMaterializedSequence={
              session.latestResponse?.materializedMessageSequence
            }
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
                <div className="thinking-dots" aria-hidden="true">
                  <span />
                  <span />
                  <span />
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
                <div className="message-content response-error-content">
                  {responseError}
                </div>
              </article>
            </div>
          ) : null}
          <div className="transcript-end-anchor" ref={transcriptEndRef} />
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
