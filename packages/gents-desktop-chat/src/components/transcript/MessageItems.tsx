import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { CopyButton } from "@source-inc/gents-desktop-ui";
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
type AssistantMessage = Extract<
  RenderedTimelineItem,
  { kind: "assistantMessage" }
>;
type PendingUserTurn = Extract<
  RenderedTimelineItem,
  { kind: "pendingUserTurn" }
>;
type LiveAssistant = Extract<RenderedTimelineItem, { kind: "liveAssistant" }>;

/** Render the bridge's canonical observation, never infer it from empty text. */
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

export function UserMessageItem({ item }: { item: UserMessage }) {
  const content = normalizeTranscriptText(item.content);
  return (
    <div className="turn-block">
      <article className="message-card user-card">
        <div className="message-role">
          user
          <MessageTime value={item.timestamp} />
          {item.reconstruction.state === "ready" && content ? (
            <CopyButton className="message-copy" getText={() => content} />
          ) : null}
        </div>
        <div className="message-content">
          <MessageAvailability reconstruction={item.reconstruction} />
          {item.reconstruction.state === "ready" ? (
            <MarkdownContent value={content} />
          ) : null}
        </div>
      </article>
    </div>
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
  if (item.reconstruction.state === "ready" && !content && !reasoning) {
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
          user
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
          <MarkdownContent value={normalizeTranscriptText(item.content)} />
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
}: {
  cause: DerivedCancelCauseView;
}) {
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
