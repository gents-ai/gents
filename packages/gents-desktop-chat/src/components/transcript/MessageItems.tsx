import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
  RequestOriginView,
} from "@source-inc/gents-desktop-client";
import { CopyButton } from "@source-inc/gents-desktop-ui";
import { CapturedContextItems } from "./CapturedContextItems.js";
import { requestProgressPresentation } from "../../chat-shell.js";

import { CancelCauseBadge, CancelCauseDetails } from "../cancelUx/index.js";
import {
  MarkdownContent,
  MessageTime,
  ReasoningDisclosure,
  RevealedMarkdownContent,
  normalizeTranscriptText,
} from "./MarkdownContent.js";

type UserMessage = Extract<RenderedTimelineItem, { kind: "userMessage" }>;
type AutomatedInput = Extract<RenderedTimelineItem, { kind: "automatedInput" }>;
type AssistantMessage = Extract<
  RenderedTimelineItem,
  { kind: "assistantMessage" }
>;
type PendingUserTurn = Extract<
  RenderedTimelineItem,
  { kind: "pendingUserTurn" }
>;
type LiveAssistant = Extract<RenderedTimelineItem, { kind: "liveAssistant" }>;

function MessageAvailability({
  reconstruction,
}: {
  reconstruction: AssistantMessage["reconstruction"];
}) {
  switch (reconstruction.state) {
    case "ready":
      return null;
    case "loading":
      return (
        <p role="status" data-testid="message-output-loading">
          Loading message…
        </p>
      );
    case "denied":
      return (
        <p role="alert" data-testid="message-output-denied">
          Message unavailable: access denied.
        </p>
      );
    case "invalid":
      return (
        <p role="alert" data-testid="message-output-invalid">
          Message could not be reconstructed.
        </p>
      );
  }
}

function originLabel(origin: RequestOriginView): string {
  switch (origin.kind) {
    case "sessionMessage":
      return "Agent message";
    case "trigger":
      return "Triggered input";
    case "goalContinuation":
      return "Goal continuation";
    case "backgroundCompletion":
      return "Background result";
  }
}

export function UserMessageItem({
  item,
  onEdit,
}: {
  item: UserMessage | AutomatedInput;
  onEdit?: (item: UserMessage) => void;
}) {
  const content = normalizeTranscriptText(item.content);
  const received =
    item.kind === "automatedInput" && item.origin.kind === "sessionMessage"
      ? item.origin
      : undefined;
  const displayContent = received
    ? normalizeTranscriptText(received.displayBody)
    : content;
  return (
    <div className="turn-block">
      <article
        className={
          received ? "message-card received-card" : "message-card user-card"
        }
      >
        <div className={received ? "received-message-heading" : "message-role"}>
          {received ? (
            <span>Agent message</span>
          ) : item.kind === "automatedInput" ? (
            originLabel(item.origin)
          ) : (
            "user"
          )}
          <MessageTime value={item.timestamp} />
          {item.kind === "userMessage" &&
          item.reconstruction.state === "ready" &&
          (content || item.capturedContext?.length) &&
          onEdit ? (
            <button
              type="button"
              className="message-edit"
              aria-label="Edit message"
              title="Edit message"
              onClick={() => onEdit(item)}
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                aria-hidden="true"
              >
                <path d="m16 3 5 5M3 21l1-6L16 3a3.5 3.5 0 0 1 5 5L9 20l-6 1Z" />
              </svg>
            </button>
          ) : null}
          {item.reconstruction.state === "ready" && displayContent ? (
            <CopyButton
              label={received ? "Copy task" : "Copy"}
              className={received ? "received-message-copy" : "message-copy"}
              getText={() =>
                received ? (received.displayBody ?? "") : content
              }
            />
          ) : null}
        </div>
        <div className="message-content">
          <MessageAvailability reconstruction={item.reconstruction} />
          {item.reconstruction.state === "ready" ? (
            <>
              {displayContent ? (
                <MarkdownContent value={displayContent} />
              ) : received ? (
                <p className="received-message-unavailable">
                  {received.displayBody == null
                    ? "Task text unavailable"
                    : "Empty message"}
                </p>
              ) : null}
              {received && item.content && (
                <details className="received-message-details">
                  <summary>Message details</summary>
                  <pre>{item.content}</pre>
                  <CopyButton
                    label="Copy original message"
                    className="received-message-copy"
                    getText={() => item.content ?? ""}
                  />
                </details>
              )}
              {item.kind === "userMessage" && (
                <CapturedContextItems contexts={item.capturedContext ?? []} />
              )}
            </>
          ) : null}
        </div>
      </article>
    </div>
  );
}

export function isEmptyAssistantMessage(item: AssistantMessage) {
  return (
    item.reconstruction.state === "ready" &&
    !normalizeTranscriptText(item.content) &&
    !normalizeTranscriptText(item.reasoning)
  );
}

export function AssistantMessageItem({
  item,
  animateReveal = false,
}: {
  item: AssistantMessage;
  animateReveal?: boolean;
}) {
  const content = normalizeTranscriptText(item.content);
  const reasoning = normalizeTranscriptText(item.reasoning);
  if (isEmptyAssistantMessage(item)) {
    return null;
  }
  return (
    <div className="turn-block">
      <article className="message-card" data-testid="assistant-message">
        <div className="message-role">
          assistant
          <MessageTime value={item.timestamp} />
          {item.reconstruction.state === "ready" && content ? (
            <CopyButton className="message-copy" getText={() => content} />
          ) : null}
        </div>
        <MessageAvailability reconstruction={item.reconstruction} />
        {item.reconstruction.state === "ready" ? (
          <ReasoningDisclosure value={reasoning} />
        ) : null}
        {item.reconstruction.state === "ready" && content ? (
          <div className="message-content">
            <RevealedMarkdownContent animate={animateReveal} value={content} />
          </div>
        ) : null}
      </article>
    </div>
  );
}

export function PendingUserTurnItem({ item }: { item: PendingUserTurn }) {
  const progress = requestProgressPresentation(item.lifecycleState);
  return (
    <div className="turn-block">
      <article className="message-card pending-card">
        <div className="message-role">
          {item.origin ? originLabel(item.origin) : "user"}
          {progress ? (
            <span
              className={`request-progress${progress.animated ? " is-active" : ""}`}
              data-testid="request-progress"
            >
              {progress.label}
            </span>
          ) : null}
        </div>
        <div className="message-content">
          {item.content && (
            <MarkdownContent value={normalizeTranscriptText(item.content)} />
          )}
          <CapturedContextItems contexts={item.capturedContext ?? []} />
        </div>
      </article>
    </div>
  );
}

export function LiveAssistantItem({ item }: { item: LiveAssistant }) {
  const content = normalizeTranscriptText(item.content);
  const reasoning = normalizeTranscriptText(item.reasoning);
  if (!content && !reasoning) {
    return null;
  }
  return (
    <article className="message-card" data-testid="assistant-message">
      <div className="message-role">
        assistant
        <span className="assistant-live-status" role="status">
          <span className="assistant-live-dot" aria-hidden="true" />
          {content ? "Responding" : "Thinking"}
        </span>
      </div>
      <ReasoningDisclosure value={reasoning} />
      {content ? (
        <div className="message-content">
          <RevealedMarkdownContent animate value={content} />
        </div>
      ) : null}
    </article>
  );
}

export function AssistantCancelCauseTurn({
  cause,
  compact = false,
}: {
  cause: DerivedCancelCauseView;
  compact?: boolean;
}) {
  if (compact)
    return (
      <details className="turn-stop">
        <summary>
          {cause.cause === "userCancelled"
            ? "Stopped by you"
            : cause.cause === "deadline"
              ? "Time limit reached"
              : cause.cause === "interrupted"
                ? "Interrupted"
                : "Stopped"}
        </summary>
        <CancelCauseDetails cause={cause} />
      </details>
    );
  return (
    <div className="turn-block">
      <article className="message-card">
        <div className="message-role">
          assistant
          <CancelCauseBadge
            cause={cause}
            className="assistant-turn-cause-badge"
          />
        </div>
        <CancelCauseDetails cause={cause} />
      </article>
    </div>
  );
}
