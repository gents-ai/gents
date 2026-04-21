import type { FormEvent } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type {
  BehaviorView,
  DeploymentView,
  DesktopSessionSnapshot,
  MessageView,
  P2PHealth,
  ToolCallView,
} from "../lib/types";
import { displayAgentIdentity, displayBehaviorLabel, formatBytes } from "../lib/types";

function MarkdownContent({ value }: { value: string }) {
  return (
    <div className="markdown-content">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{value}</ReactMarkdown>
    </div>
  );
}

function normalizeTranscriptText(value?: string | null) {
  if (!value) {
    return "";
  }

  return value.replace(/\n{3,}/g, "\n\n").trim();
}

function stripRepeatedAssistantPrefix(
  previousAssistantText: string | null,
  currentAssistantText: string,
) {
  if (!previousAssistantText) {
    return currentAssistantText;
  }

  const normalizedPrevious = normalizeTranscriptText(previousAssistantText);
  const normalizedCurrent = normalizeTranscriptText(currentAssistantText);

  if (
    normalizedPrevious &&
    normalizedCurrent.length > normalizedPrevious.length &&
    normalizedCurrent.startsWith(normalizedPrevious)
  ) {
    return normalizedCurrent.slice(normalizedPrevious.length).trimStart();
  }

  return normalizedCurrent;
}

function parseStructuredValue(value?: string | null) {
  if (!value) {
    return null;
  }

  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }

  try {
    return JSON.parse(trimmed) as unknown;
  } catch {
    return null;
  }
}

function renderScalar(value: unknown) {
  if (value == null) {
    return "";
  }
  if (typeof value === "string") {
    return value;
  }
  if (
    typeof value === "number" ||
    typeof value === "boolean" ||
    typeof value === "bigint"
  ) {
    return String(value);
  }
  return JSON.stringify(value, null, 2);
}

function toolStatusClass(status?: string | null) {
  switch ((status ?? "").toLowerCase()) {
    case "completed":
    case "complete":
    case "success":
      return "tool-item-dot tool-item-dot-success";
    case "failed":
    case "error":
    case "cancelled":
      return "tool-item-dot tool-item-dot-error";
    default:
      return "tool-item-dot tool-item-dot-running";
  }
}

type ChatWorkspaceProps = {
  running: boolean;
  starting: boolean;
  selectedDeployment: DeploymentView | null;
  selectedConversationTitle: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  session: DesktopSessionSnapshot | null;
  sessionTools: ToolCallView[];
  behaviorOptions: BehaviorView[];
  runtimeHealth: P2PHealth | null;
  rowCount: number;
  approxSerializedBytes: number;
  dialedPeerCount: number;
  configuredPeerCount: number;
  draft: string;
  sending: boolean;
  onStart: () => void;
  onSelectBehavior: (behaviorId: string) => void;
  onDraftChange: (value: string) => void;
  onSend: (event: FormEvent) => void;
};

function toolGroupKey(messageSequence?: number | null) {
  return messageSequence ?? -1;
}

function ToolDetailSection({
  label,
  value,
}: {
  label: string;
  value?: string | null;
}) {
  if (!value?.trim()) {
    return null;
  }

  const parsed = parseStructuredValue(value);
  const entries =
    parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? Object.entries(parsed as Record<string, unknown>)
      : null;

  return (
    <div className="tool-detail">
      <div className="tool-detail-label">{label}</div>
      {entries ? (
        <div className="tool-detail-grid">
          {entries.map(([key, entryValue]) => (
            <div className="tool-detail-row" key={key}>
              <div className="tool-detail-key">{key}</div>
              <div className="tool-detail-value">{renderScalar(entryValue)}</div>
            </div>
          ))}
        </div>
      ) : (
        <pre className="tool-block">{value}</pre>
      )}
    </div>
  );
}

function ToolGroups({ tools }: { tools: ToolCallView[] }) {
  return (
    <section className="tool-group">
      <div className="tool-group-meta">
        <span className="tool-group-label">
          Tool Calls
        </span>
        <span className="muted small">
          {tools.length} tool {tools.length === 1 ? "call" : "calls"}
        </span>
      </div>
      {tools.map((tool) => (
        <details className="tool-item" key={tool.toolCallKey}>
          <summary className="tool-item-summary">
            <span className="tool-item-summary-left">
              <span aria-hidden="true" className={toolStatusClass(tool.status)} />
              <span className="tool-item-name">{tool.toolName ?? "tool"}</span>
            </span>
            <span className="tool-item-action">View</span>
          </summary>
          <div className="tool-item-body">
            <ToolDetailSection label="args" value={tool.args} />
            <ToolDetailSection label="result" value={tool.result} />
          </div>
        </details>
      ))}
    </section>
  );
}

function MessageList({
  messages,
  tools,
  activeResponseOverlay,
}: {
  messages: MessageView[];
  tools: ToolCallView[];
  activeResponseOverlay?: DesktopSessionSnapshot["activeResponseOverlay"];
}) {
  const timelineMessages = [...messages]
    .filter(
      (message) =>
        !message.hasToolResults &&
        Boolean(
          normalizeTranscriptText(message.displayContent) ||
            normalizeTranscriptText(message.reasoning) ||
            message.hasToolCalls,
        ),
    )
    .sort((left, right) => (left.sequence ?? 0) - (right.sequence ?? 0));

  const toolGroups = new Map<number, ToolCallView[]>();
  for (const tool of [...tools].sort((left, right) => {
    const seqOrder = (left.messageSequence ?? 0) - (right.messageSequence ?? 0);
    if (seqOrder !== 0) {
      return seqOrder;
    }
    return left.toolCallKey.localeCompare(right.toolCallKey);
  })) {
    const key = toolGroupKey(tool.messageSequence);
    const existing = toolGroups.get(key) ?? [];
    existing.push(tool);
    toolGroups.set(key, existing);
  }

  const usedToolKeys = new Set<number>();
  let lastAssistantText: string | null = null;

  return (
    <>
      {timelineMessages.map((message) => {
        const groupKey = toolGroupKey(message.sequence);
        const groupedTools = toolGroups.get(groupKey) ?? [];
        usedToolKeys.add(groupKey);
        const normalizedReasoning = normalizeTranscriptText(message.reasoning);
        let normalizedContent = normalizeTranscriptText(message.displayContent);
        if ((message.displayRole ?? message.role) === "assistant") {
          normalizedContent = stripRepeatedAssistantPrefix(
            lastAssistantText,
            normalizedContent,
          );
          if (normalizeTranscriptText(message.displayContent ?? message.content)) {
            lastAssistantText = normalizeTranscriptText(message.displayContent);
          }
        }
        const hasVisibleBody = Boolean(
          normalizedContent || normalizedReasoning,
        );

        return (
          <div className="turn-block" key={message.messageKey}>
            {hasVisibleBody ? (
              <article className="message-card">
                <div className="message-role">
                  {message.displayRole ?? message.role ?? "assistant"}
                </div>
                {normalizedReasoning ? (
                  <div className="message-reasoning">
                    <MarkdownContent value={normalizedReasoning} />
                  </div>
                ) : null}
                {normalizedContent ? (
                  <div className="message-content">
                    <MarkdownContent value={normalizedContent} />
                  </div>
                ) : null}
              </article>
            ) : null}
            {groupedTools.length ? <ToolGroups tools={groupedTools} /> : null}
          </div>
        );
      })}

      {[...toolGroups.entries()]
        .filter(([key]) => !usedToolKeys.has(key))
        .map(([key, groupedTools]) => (
          <div className="turn-block" key={`tools-${key}`}>
            <ToolGroups tools={groupedTools} />
          </div>
        ))}

      {activeResponseOverlay?.content ? (
        <article className="message-card response-card">
          <div className="message-role">assistant</div>
          <div className="message-content">
            <MarkdownContent
              value={normalizeTranscriptText(activeResponseOverlay.content)}
            />
          </div>
        </article>
      ) : null}
    </>
  );
}

export function ChatWorkspace({
  running,
  starting,
  selectedDeployment,
  selectedConversationTitle,
  selectedBehaviorId,
  selectedSessionId,
  session,
  sessionTools,
  behaviorOptions,
  runtimeHealth,
  rowCount,
  approxSerializedBytes,
  dialedPeerCount,
  configuredPeerCount,
  draft,
  sending,
  onStart,
  onSelectBehavior,
  onDraftChange,
  onSend,
}: ChatWorkspaceProps) {
  if (!running) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Chat</p>
        <h2>Start the desktop core</h2>
        <p className="lede compact">
          The Tauri shell now knows how to initialize the existing desktop
          runtime and hold a live Rust client core. Start it to debug the
          initial chat screen.
        </p>
        <button className="primary-button" disabled={starting} onClick={onStart}>
          {starting ? "Starting…" : "Start Desktop Core"}
        </button>
      </article>
    );
  }

  if (!selectedDeployment) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Chat</p>
        <h2>Select a deployment</h2>
        <p className="muted">
          Pick a saved deployment from the left rail to debug the first chat
          screen.
        </p>
      </article>
    );
  }

  const displayAgentDid = displayAgentIdentity(selectedDeployment.agentDid);
  const displayBehavior = displayBehaviorLabel(
    selectedBehaviorId ?? selectedDeployment.defaultBehaviorId ?? null,
  );

  return (
    <>
      <header className="chat-header">
        <div>
          <h2>{selectedConversationTitle ?? "New Conversation"}</h2>
          {displayAgentDid ? <p className="muted mono">{displayAgentDid}</p> : null}
        </div>
        <div className="chat-status">
          {displayBehavior ? <span className="chip">{displayBehavior}</span> : null}
          <span
            className={
              runtimeHealth?.status === "healthy" ? "chip chip-green" : "chip"
            }
          >
            {runtimeHealth?.status ?? "unknown"}
          </span>
        </div>
      </header>

      <section className="chat-workspace">
        <div className="chat-main">
          <section className="panel transcript-panel">
            {selectedSessionId && session ? (
              <div className="message-list">
                <MessageList
                  messages={session.messages}
                  activeResponseOverlay={session.activeResponseOverlay}
                  tools={sessionTools}
                />
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

          <form className="panel composer-panel" onSubmit={onSend}>
            <div className="composer-toolbar">
              <div className="behavior-chips">
                {behaviorOptions.map((behavior) => (
                  <button
                    className={
                      behavior.behaviorId === selectedBehaviorId
                        ? "chip chip-button selected"
                        : "chip chip-button"
                    }
                    key={behavior.behaviorId}
                    onClick={(event) => {
                      event.preventDefault();
                      onSelectBehavior(behavior.behaviorId);
                    }}
                    type="button"
                  >
                    {behavior.displayName}
                  </button>
                ))}
              </div>
              <div className="muted small">
                {rowCount} rows / {formatBytes(approxSerializedBytes)}
              </div>
            </div>

            <textarea
              className="composer-input"
              onChange={(event) => onDraftChange(event.currentTarget.value)}
              placeholder="Message the selected agent"
              value={draft}
            />

            <div className="composer-footer">
              <div className="muted small">
                {session?.turnState ?? "idle"} · peers {dialedPeerCount}/{configuredPeerCount}
              </div>
              <button
                className="primary-button"
                disabled={sending || !draft.trim()}
                type="submit"
              >
                {sending ? "Sending…" : "Send"}
              </button>
            </div>
          </form>
        </div>
      </section>
    </>
  );
}
