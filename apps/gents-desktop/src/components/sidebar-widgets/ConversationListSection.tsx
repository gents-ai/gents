import { useEffect, useMemo, useState } from "react";

import { conversationBelongsToBehavior } from "@source-inc/gents-desktop-chat";
import type {
  ConversationSummary,
  DeploymentView,
} from "@source-inc/gents-desktop-client";
import { displayConversationTitle } from "@source-inc/gents-desktop-client";
import { formatRelativeTime, PencilIcon } from "@source-inc/gents-desktop-fleet";
import { conversationStatusClass } from "./sidebarUtils";

export type ConversationListSectionProps = {
  conversations: ConversationSummary[];
  deployments: DeploymentView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onOpenSession?: (sessionId: string) => void;
  onRenameConversationTitle?: (
    sessionId: string,
    title: string,
  ) => void | Promise<void>;
  onSyncConversations?: () => Promise<unknown> | void;
  syncingConversations?: boolean;
};

export function ConversationListSection({
  conversations,
  deployments,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionId,
  onSelectSession,
  onOpenSession,
  onRenameConversationTitle,
  onSyncConversations,
  syncingConversations = false,
}: ConversationListSectionProps) {
  const [query, setQuery] = useState("");
  const [renamingSessionId, setRenamingSessionId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [savingRename, setSavingRename] = useState(false);

  async function commitRename() {
    if (savingRename) {
      return;
    }
    const title = renameDraft.trim();
    const sessionId = renamingSessionId;
    const conversation = conversations.find(
      (candidate) => candidate.sessionId === sessionId,
    );
    if (
      !sessionId ||
      !title ||
      !onRenameConversationTitle ||
      title === displayConversationTitle(conversation?.title).trim()
    ) {
      setRenamingSessionId(null);
      return;
    }

    setSavingRename(true);
    try {
      await onRenameConversationTitle(sessionId, title);
      setRenamingSessionId(null);
    } catch {
    } finally {
      setSavingRename(false);
    }
  }
  const selectedDeployment = deployments.find(
    (item) => item.agentDid === selectedAgentDid,
  );
  const selectedDeploymentLabel = selectedDeployment?.label ?? "Chat";
  const defaultBehaviorId =
    selectedDeployment?.defaultBehaviorId ??
    selectedDeployment?.behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
    null;
  const behaviorConversations = useMemo(
    () =>
      conversations.filter((conversation) =>
        conversationBelongsToBehavior(
          conversation,
          selectedBehaviorId,
          defaultBehaviorId,
        ),
      ),
    [conversations, defaultBehaviorId, selectedBehaviorId],
  );
  useEffect(() => {
    setQuery("");
    setRenamingSessionId(null);
    setSavingRename(false);
  }, [selectedAgentDid]);

  const filteredConversations = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) {
      return behaviorConversations;
    }
    return behaviorConversations.filter((conversation) =>
      `${displayConversationTitle(conversation.title)} ${conversation.previewText ?? ""}`
        .toLowerCase()
        .includes(needle),
    );
  }, [behaviorConversations, query]);

  return (
    <section className="sidebar-section conversation-section">
      <div className="panel-header">
        <div>
          <p className="eyebrow">Conversations</p>
          <h2>{selectedDeploymentLabel}</h2>
        </div>
        <div className="sidebar-section-controls">
          {behaviorConversations.length > 1 ? (
            <span className="sidebar-scroll-hint">
              {behaviorConversations.length} · swipe
            </span>
          ) : null}
          {onSyncConversations ? (
            <button
              className="ghost-button conversation-sync"
              data-testid="conversation-sync-p2p"
              disabled={!selectedAgentDid || syncingConversations}
              onClick={() => {
                void Promise.resolve(onSyncConversations()).catch(() => {});
              }}
              title="Reconnect the signed P2P replica and sync conversations"
              type="button"
            >
              {syncingConversations ? "Syncing…" : "Sync P2P"}
            </button>
          ) : null}
        </div>
      </div>
      {selectedAgentDid && behaviorConversations.length > 0 ? (
        <input
          className="conversation-search"
          data-testid="conversation-search"
          onChange={(event) => setQuery(event.currentTarget.value)}
          placeholder="Search conversations"
          type="search"
          value={query}
        />
      ) : null}
      {!selectedAgentDid ? (
        <p className="muted">Select a deployment to see conversations.</p>
      ) : !behaviorConversations.length ? (
        <p className="muted">
          No conversations for this behavior yet. Sending the first message will create
          one automatically.
        </p>
      ) : !filteredConversations.length ? (
        <p className="muted">No conversations match the search.</p>
      ) : (
        <div className="list conversation-list">
          {filteredConversations.map((conversation) => {
            const when = conversation.updatedAt ?? conversation.createdAt;
            if (conversation.sessionId === renamingSessionId) {
              return (
                <input
                  aria-label={`Rename ${displayConversationTitle(conversation.title)}`}
                  autoFocus
                  className="conversation-rename-input"
                  data-testid={`conversation-rename-input-${conversation.sessionId}`}
                  disabled={savingRename}
                  key={conversation.sessionId}
                  onBlur={() => void commitRename()}
                  onChange={(event) => setRenameDraft(event.currentTarget.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      void commitRename();
                    } else if (event.key === "Escape") {
                      setRenamingSessionId(null);
                    }
                  }}
                  value={renameDraft}
                />
              );
            }
            return (
              <span className="conversation-row" key={conversation.sessionId}>
                <button
                  className={
                    conversation.sessionId === selectedSessionId
                      ? "list-item selected"
                      : "list-item"
                  }
                  data-testid={`conversation-${conversation.sessionId}`}
                  onClick={() =>
                    (onOpenSession ?? onSelectSession)(conversation.sessionId)
                  }
                  type="button"
                >
                  <span className="conversation-list-row">
                    <span
                      aria-hidden="true"
                      className={conversationStatusClass(conversation)}
                    />
                    <span
                      className={
                        conversation.title
                          ? "list-item-title conversation-list-title"
                          : "list-item-title conversation-list-title untitled-title"
                      }
                    >
                      {displayConversationTitle(conversation.title)}
                    </span>
                    {when ? (
                      <span className="conversation-time" title={when}>
                        {formatRelativeTime(when)}
                      </span>
                    ) : null}
                  </span>
                  {conversation.taskId ? (
                    <span
                      className="conversation-task-tag"
                      title={displayConversationTaskLabel(conversation)}
                    >
                      {displayConversationTaskLabel(conversation)}
                    </span>
                  ) : null}
                </button>
                {onRenameConversationTitle ? (
                  <button
                    aria-label={`Rename ${displayConversationTitle(conversation.title)}`}
                    className="ghost-button conversation-rename"
                    data-testid={`conversation-rename-${conversation.sessionId}`}
                    disabled={savingRename}
                    onClick={() => {
                      setRenameDraft(displayConversationTitle(conversation.title));
                      setRenamingSessionId(conversation.sessionId);
                    }}
                    title="Rename conversation"
                    type="button"
                  >
                    <PencilIcon />
                  </button>
                ) : null}
              </span>
            );
          })}
        </div>
      )}
    </section>
  );
}

function displayConversationTaskLabel(conversation: ConversationSummary) {
  const name = conversation.taskName?.trim();
  return name && name.length > 0 ? name : (conversation.taskId ?? "Task");
}
