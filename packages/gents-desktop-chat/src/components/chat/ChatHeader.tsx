import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  DesktopSessionSnapshot,
  P2PHealth,
} from "@source-inc/gents-desktop-client";
import { displayConversationTitle } from "@source-inc/gents-desktop-client";

export type ChatHeaderProps = {
  behaviorLabel: string | null;
  runtimeHealth: P2PHealth | null;
  configuredPeerCount?: number;
  dialedPeerCount?: number;
  context?: DesktopSessionSnapshot["context"] | null;
  selectedConversationTitle: string | null;
  selectedSessionId: string | null;
  onRenameConversationTitle: (
    sessionId: string,
    title: string,
  ) => void | Promise<void>;
  onOpenMobileNavigation?: () => void;
};

function formatTokens(value: number) {
  if (value < 1_000) return String(value);
  if (value < 1_000_000) {
    return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)}k`;
  }
  return `${(value / 1_000_000).toFixed(1)}m`;
}

function ContextMeter({
  context,
}: {
  context: DesktopSessionSnapshot["context"];
}) {
  const lastRequest = context.lastRequest;
  const used = Math.max(
    0,
    lastRequest?.estimatedInputTokens ?? context.estimatedConversationTokens,
  );
  const durable = Math.max(0, context.estimatedDurableTokens);
  const conversation = Math.max(0, context.estimatedConversationTokens);
  const window = Math.max(
    1,
    lastRequest?.contextWindow ?? context.contextWindow,
  );
  const threshold = Math.max(
    0,
    lastRequest?.compactionThresholdTokens ?? context.compactionThresholdTokens,
  );
  const usedPercent = Math.min(100, (used / window) * 100);
  const thresholdPercent = Math.min(100, (threshold / window) * 100);
  const displayedThresholdPercent = Math.round((threshold / window) * 100);
  const projectedAway = Math.max(0, durable - conversation);
  const projectedAwayPercent = durable
    ? Math.round((projectedAway / durable) * 100)
    : 0;
  const recentCompactions = context.compactions.slice(-3).reverse();
  const transcriptTotalsExact = context.transcriptTotalsExact !== false;
  const title = lastRequest
    ? `Last assembled provider input: ${used.toLocaleString()} of ` +
      `${window.toLocaleString()} tokens. Compaction decision: ` +
      `${lastRequest.compactionReason}.`
    : `${transcriptTotalsExact ? "Estimated" : "At least"} durable conversation context: ${used.toLocaleString()} of ` +
      `${window.toLocaleString()} tokens. Compaction threshold: ` +
      `${threshold.toLocaleString()} tokens.`;
  const compactionDecision = lastRequest
    ? ({
        below_threshold: "Below threshold",
        compacted: "Compacted",
        compactor_unavailable: "Compactor unavailable",
      }[lastRequest.compactionReason] ?? lastRequest.compactionReason)
    : null;

  return (
    <details className="context-meter" data-testid="context-meter">
      <summary className="chip" title={title}>
        Context ≈{formatTokens(used)} / {formatTokens(window)}
      </summary>
      <div
        className="context-meter-popover mobile-viewport-popover"
        data-scroll-owner="popover"
      >
        <div className="context-meter-heading">
          <strong>
            {lastRequest ? "Last provider request" : "Conversation context"}
          </strong>
          <span>{used.toLocaleString()} estimated tokens</span>
        </div>
        <div
          aria-label={`${used.toLocaleString()} of ${window.toLocaleString()} context tokens`}
          aria-valuemax={window}
          aria-valuemin={0}
          aria-valuenow={Math.min(used, window)}
          className="context-meter-track"
          role="progressbar"
        >
          <span
            className="context-meter-fill"
            style={{ width: `${usedPercent}%` }}
          />
          <span
            aria-hidden="true"
            className="context-meter-threshold"
            style={{ left: `${thresholdPercent}%` }}
          />
        </div>
        <dl className="context-meter-facts">
          {lastRequest ? (
            <>
              <div>
                <dt>Decision</dt>
                <dd>{compactionDecision}</dd>
              </div>
              <div>
                <dt>Before compaction</dt>
                <dd>
                  {lastRequest.preCompactionInputTokens?.toLocaleString() ??
                    "Not needed"}
                </dd>
              </div>
              <div>
                <dt>Messages</dt>
                <dd>{lastRequest.components.messages.toLocaleString()}</dd>
              </div>
              <div>
                <dt>Tool schemas</dt>
                <dd>{lastRequest.components.toolSchemas.toLocaleString()}</dd>
              </div>
              <div>
                <dt>Other request layers</dt>
                <dd>
                  {(
                    lastRequest.components.documents +
                    lastRequest.components.additionalParameters +
                    lastRequest.components.outputSchema
                  ).toLocaleString()}
                </dd>
              </div>
              <div>
                <dt>Output allowance</dt>
                <dd>
                  {lastRequest.effectiveMaxOutputTokens?.toLocaleString() ??
                    "Provider default"}
                </dd>
              </div>
            </>
          ) : null}
          <div>
            <dt>Conversation projection</dt>
            <dd>{conversation.toLocaleString()}</dd>
          </div>
          <div>
            <dt>Projected away</dt>
            <dd>
              {projectedAway.toLocaleString()} ({projectedAwayPercent}%)
            </dd>
          </div>
          <div>
            <dt>Context window</dt>
            <dd>{window.toLocaleString()}</dd>
          </div>
          <div>
            <dt>Compacts at</dt>
            <dd>
              {threshold.toLocaleString()} ({displayedThresholdPercent}%)
            </dd>
          </div>
          <div>
            <dt>Provider view</dt>
            <dd>
              {transcriptTotalsExact ? "" : "at least "}
              {context.providerMessageCount.toLocaleString()} /{" "}
              {context.durableMessageCount.toLocaleString()} messages
            </dd>
          </div>
          <div>
            <dt>Strategy</dt>
            <dd>{context.compactionStrategy}</dd>
          </div>
        </dl>
        <div className="context-meter-history">
          <strong>
            {context.compactions.length
              ? `${context.compactions.length} durable compaction${
                  context.compactions.length === 1 ? "" : "s"
                }`
              : "No durable compactions yet"}
          </strong>
          {recentCompactions.map((compaction) => (
            <div className="context-meter-event" key={compaction.compactionKey}>
              <span>sequence {compaction.sequence ?? "?"}</span>
              <span>
                {compaction.originalTokens?.toLocaleString() ?? "?"} →{" "}
                {compaction.compactedTokens?.toLocaleString() ?? "?"} tokens
              </span>
              <span>
                {compaction.messagesCompacted.toLocaleString()} messages
              </span>
            </div>
          ))}
        </div>
        <p className="context-meter-note">
          {lastRequest
            ? "The last assembled input is an immutable request-bound runtime estimate, including fixed instructions, tools, parameters, and the current prompt. “Projected away” compares the durable transcript with its current provider-view projection."
            : "Runtime estimate of durable provider-visible conversation state. Fixed instructions, tool schemas, selected skills, and the next message are added per request. “Projected away” includes provider-view narrowing and durable summary compactions."}
        </p>
      </div>
    </details>
  );
}

export function p2pConnectionDisplay(
  runtimeHealth: P2PHealth | null,
  configuredPeerCount: number,
  dialedPeerCount: number,
) {
  const status = runtimeHealth?.status ?? "unknown";
  const title = runtimeHealth
    ? `Transport ${status}; ${dialedPeerCount}/${configuredPeerCount} saved peers dialed; ${runtimeHealth.connectedPeerCount} active connections; ${runtimeHealth.replicatorCount} replicators`
    : `Checking P2P transport; ${dialedPeerCount}/${configuredPeerCount} saved peers dialed`;

  if (!runtimeHealth) {
    return { label: "Checking sync", healthy: false, title };
  }
  if (runtimeHealth.status === "wedged") {
    return { label: "P2P stalled", healthy: false, title };
  }
  if (runtimeHealth.status !== "healthy") {
    return { label: "P2P retrying", healthy: false, title };
  }
  if (configuredPeerCount === 0) {
    return { label: "Local", healthy: true, title };
  }
  if (dialedPeerCount < configuredPeerCount) {
    return {
      label: `Reconnecting ${dialedPeerCount}/${configuredPeerCount}`,
      healthy: false,
      title,
    };
  }
  return { label: "Paired", healthy: true, title };
}

export function ChatHeader({
  behaviorLabel,
  runtimeHealth,
  configuredPeerCount = 0,
  dialedPeerCount = 0,
  context,
  selectedConversationTitle,
  selectedSessionId,
  onRenameConversationTitle,
  onOpenMobileNavigation,
}: ChatHeaderProps) {
  const p2pDisplay = p2pConnectionDisplay(
    runtimeHealth,
    configuredPeerCount,
    dialedPeerCount,
  );
  const visibleConversationTitle = selectedSessionId
    ? displayConversationTitle(selectedConversationTitle)
    : "Start a conversation";
  const [isRenamingTitle, setIsRenamingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState(selectedConversationTitle ?? "");
  const [renamingTitle, setRenamingTitle] = useState(false);

  useEffect(() => {
    setIsRenamingTitle(false);
    setTitleDraft(selectedConversationTitle ?? "");
  }, [selectedConversationTitle, selectedSessionId]);

  async function submitTitleRename(event?: FormEvent) {
    event?.preventDefault();
    if (!selectedSessionId) {
      return;
    }

    const trimmed = titleDraft.trim();
    if (!trimmed) {
      setIsRenamingTitle(false);
      setTitleDraft(selectedConversationTitle ?? "");
      return;
    }

    if (trimmed === (selectedConversationTitle ?? "").trim()) {
      setIsRenamingTitle(false);
      return;
    }

    setRenamingTitle(true);
    try {
      await onRenameConversationTitle(selectedSessionId, trimmed);
      setIsRenamingTitle(false);
    } catch {
    } finally {
      setRenamingTitle(false);
    }
  }

  return (
    <header className="chat-header">
      <div className="chat-title-block">
        {onOpenMobileNavigation ? (
          <button
            className="ghost-button mobile-chat-navigation-button"
            data-testid="mobile-chat-navigation"
            onClick={onOpenMobileNavigation}
            type="button"
          >
            <span aria-hidden="true">←</span>
            Chats
          </button>
        ) : null}
        {selectedSessionId ? (
          isRenamingTitle ? (
            <form className="title-rename-form" onSubmit={submitTitleRename}>
              <input
                aria-label={`Rename ${visibleConversationTitle}`}
                autoFocus
                className="title-rename-input"
                data-testid="conversation-title-input"
                onBlur={() => void submitTitleRename()}
                onChange={(event) => setTitleDraft(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    setIsRenamingTitle(false);
                    setTitleDraft(selectedConversationTitle ?? "");
                  }
                }}
                value={titleDraft}
              />
            </form>
          ) : (
            <div className="chat-title-row">
              <h2>{visibleConversationTitle}</h2>
              <button
                aria-label={`Rename ${visibleConversationTitle}`}
                className="icon-button"
                data-testid="conversation-title-edit"
                disabled={renamingTitle}
                onClick={() => setIsRenamingTitle(true)}
                type="button"
              >
                Edit
              </button>
            </div>
          )
        ) : (
          <h2>{visibleConversationTitle}</h2>
        )}
      </div>
      <div className="chat-status">
        {behaviorLabel ? <span className="chip">{behaviorLabel}</span> : null}
        {context ? <ContextMeter context={context} /> : null}
        <span
          className={p2pDisplay.healthy ? "chip chip-green" : "chip"}
          title={p2pDisplay.title}
        >
          {p2pDisplay.label}
        </span>
      </div>
    </header>
  );
}
