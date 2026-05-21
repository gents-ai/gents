import { useEffect, useMemo, useRef, useState } from "react";

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
                return [
                  item.content?.length ?? 0,
                  item.reasoning?.length ?? 0,
                ];
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

  useEffect(() => {
    if (!autoFollowTranscript) {
      return;
    }

    const scrollTarget = transcriptEndRef.current;
    if (!scrollTarget) {
      return;
    }

    const frame = window.requestAnimationFrame(() => {
      scrollTarget.scrollIntoView({ block: "end" });
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

  return (
    <section
      className="panel transcript-panel"
      data-testid="transcript-panel"
      onScroll={handleTranscriptScroll}
      ref={transcriptPanelRef}
    >
      {selectedSessionId && session ? (
        <div className="message-list">
          <MessageList
            timelineItems={session.timelineItems}
            responseCancelCause={session.latestResponse?.cancelCause}
            responseMaterializedSequence={session.latestResponse?.materializedMessageSequence}
          />
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
