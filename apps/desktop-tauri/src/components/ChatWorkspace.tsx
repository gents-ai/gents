import { useEffect, useMemo, useRef, useState } from "react";
import type { FormEvent, KeyboardEvent } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type {
  BehaviorView,
  DeploymentView,
  DesktopSessionSnapshot,
  P2PHealth,
  RenderedTimelineItem,
  RenderedToolCallView,
  ToolDetailValueView,
} from "../lib/types";
import {
  displayAgentIdentity,
  displayBehaviorLabel,
  displayConversationTitle,
  formatBytes,
} from "../lib/types";

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

  return value.trim();
}

function toolStatusClass(statusKind?: string | null) {
  switch ((statusKind ?? "").toLowerCase()) {
    case "success":
      return "tool-item-dot tool-item-dot-success";
    case "error":
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
  behaviorOptions: BehaviorView[];
  runtimeHealth: P2PHealth | null;
  rowCount: number;
  approxSerializedBytes: number;
  dialedPeerCount: number;
  configuredPeerCount: number;
  canSend: boolean;
  sendHint: string | null;
  draft: string;
  sending: boolean;
  onStart: () => void;
  onRenameConversationTitle: (sessionId: string, title: string) => void | Promise<void>;
  onSelectBehavior: (behaviorId: string) => void;
  onDraftChange: (value: string) => void;
  onSend: (event: FormEvent) => void;
};

function ToolDetailSection({
  label,
  value,
}: {
  label: string;
  value?: ToolDetailValueView | null;
}) {
  if (!value?.rawText.trim()) {
    return null;
  }

  return (
    <div className="tool-detail">
      <div className="tool-detail-label">{label}</div>
      {value.fields.length > 0 ? (
        <div className="tool-detail-grid">
          {value.fields.map((field) => (
            <div className="tool-detail-row" key={field.key}>
              <div className="tool-detail-key">{field.key}</div>
              <div className="tool-detail-value">{field.value}</div>
            </div>
          ))}
        </div>
      ) : (
        <pre className="tool-block">{value.rawText}</pre>
      )}
    </div>
  );
}

function ToolGroups({ tools }: { tools: RenderedToolCallView[] }) {
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
        <details className="tool-item" key={tool.itemKey}>
          <summary className="tool-item-summary">
            <span className="tool-item-summary-left">
              <span aria-hidden="true" className={toolStatusClass(tool.statusKind)} />
              <span className="tool-item-name">{tool.toolName}</span>
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

function ReasoningDisclosure({
  value,
  summary = "Thinking",
}: {
  value?: string | null;
  summary?: string;
}) {
  const normalized = normalizeTranscriptText(value);
  if (!normalized) {
    return null;
  }

  return (
    <details className="reasoning-disclosure">
      <summary className="reasoning-summary">{summary}</summary>
      <div className="message-reasoning">
        <MarkdownContent value={normalized} />
      </div>
    </details>
  );
}

function MessageList({
  timelineItems,
}: {
  timelineItems: RenderedTimelineItem[];
}) {
  return (
    <>
      {timelineItems.map((item) => {
        switch (item.kind) {
          case "userMessage":
            return (
              <div className="turn-block" key={item.itemKey}>
                <article className="message-card">
                  <div className="message-role">user</div>
                  <div className="message-content">
                    <MarkdownContent value={normalizeTranscriptText(item.content)} />
                  </div>
                </article>
              </div>
            );
          case "assistantMessage": {
            const normalizedContent = normalizeTranscriptText(item.content);
            const normalizedReasoning = normalizeTranscriptText(item.reasoning);
            if (!normalizedContent && !normalizedReasoning) {
              return null;
            }
            return (
              <div className="turn-block" key={item.itemKey}>
                <article className="message-card">
                  <div className="message-role">assistant</div>
                  {normalizedContent ? (
                    <div className="message-content">
                      <MarkdownContent value={normalizedContent} />
                    </div>
                  ) : null}
                  <ReasoningDisclosure value={normalizedReasoning} />
                </article>
              </div>
            );
          }
          case "toolGroup":
            return (
              <div className="turn-block" key={item.itemKey}>
                <ToolGroups tools={item.tools} />
              </div>
            );
          case "pendingUserTurn":
            return (
              <div className="turn-block" key={item.itemKey}>
                <article className="message-card pending-card">
                  <div className="message-role">user</div>
                  <div className="message-content">
                    <MarkdownContent value={normalizeTranscriptText(item.content)} />
                  </div>
                </article>
              </div>
            );
          case "liveAssistant": {
            const overlayContent = normalizeTranscriptText(item.content);
            const overlayReasoning = normalizeTranscriptText(item.reasoning);
            if (!overlayContent && !overlayReasoning) {
              return null;
            }
            return (
              <article className="message-card" key={item.itemKey}>
                <div className="message-role">assistant</div>
                {overlayContent ? (
                  <div className="message-content">
                    <MarkdownContent value={overlayContent} />
                  </div>
                ) : null}
                <ReasoningDisclosure value={overlayReasoning} />
              </article>
            );
          }
          default:
            return null;
        }
      })}
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
  behaviorOptions,
  runtimeHealth,
  rowCount,
  approxSerializedBytes,
  dialedPeerCount,
  configuredPeerCount,
  canSend,
  sendHint,
  draft,
  sending,
  onStart,
  onRenameConversationTitle,
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
  const visibleConversationTitle = selectedSessionId
    ? displayConversationTitle(selectedConversationTitle)
    : "Start a conversation";
  const transcriptPanelRef = useRef<HTMLElement | null>(null);
  const transcriptEndRef = useRef<HTMLDivElement | null>(null);
  const [autoFollowTranscript, setAutoFollowTranscript] = useState(true);
  const [isRenamingTitle, setIsRenamingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState(selectedConversationTitle ?? "");
  const [renamingTitle, setRenamingTitle] = useState(false);

  const transcriptSignature = useMemo(
    () =>
      JSON.stringify({
        sessionId: selectedSessionId,
        timelineLength: session?.timelineItems.length ?? 0,
        timelineKinds: session?.timelineItems.map((item) => item.kind) ?? [],
        turnState: session?.turnState ?? "",
      }),
    [
      selectedSessionId,
      session?.timelineItems,
      session?.turnState,
    ],
  );

  useEffect(() => {
    setAutoFollowTranscript(true);
  }, [selectedSessionId]);

  useEffect(() => {
    setIsRenamingTitle(false);
    setTitleDraft(selectedConversationTitle ?? "");
  }, [selectedConversationTitle, selectedSessionId]);

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

    const remaining =
      panel.scrollHeight - panel.scrollTop - panel.clientHeight;
    setAutoFollowTranscript(remaining < 64);
  }

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
      // shell surfaces the error banner; keep the inline editor open for correction
    } finally {
      setRenamingTitle(false);
    }
  }

  function onComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (
      event.key !== "Enter" ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.nativeEvent.isComposing ||
      !canSend
    ) {
      return;
    }

    event.preventDefault();
    event.currentTarget.form?.requestSubmit();
  }

  return (
    <>
      <header className="chat-header">
        <div className="chat-title-block">
          {selectedSessionId ? (
            isRenamingTitle ? (
              <form className="title-rename-form" onSubmit={submitTitleRename}>
                <input
                  autoFocus
                  className="title-rename-input"
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
                  className="icon-button"
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
                />
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

          <form
            className="panel composer-panel"
            data-testid="composer-form"
            onSubmit={onSend}
          >
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
              onKeyDown={onComposerKeyDown}
              placeholder="Message the selected agent"
              data-testid="composer-input"
              value={draft}
            />

            <div className="composer-footer">
              <div className="muted small">
                {sendHint ?? session?.turnState ?? "idle"} · peers {dialedPeerCount}/{configuredPeerCount}
              </div>
              <button
                className="primary-button"
                data-testid="composer-send"
                disabled={!canSend}
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
