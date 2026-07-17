import { useEffect, useMemo, useState } from "react";

import type { ConversationSummary, DeploymentView, TaskView } from "../../lib/types";
import { displayConversationTitle } from "../../lib/types";
import { formatRelativeTime } from "../fleet/fleetMetrics";
import { PencilIcon } from "../fleet/FleetIcons";
import { conversationStatusClass } from "./sidebarUtils";

const ALL_TASKS_FILTER = "__all__";
const UNTASKED_FILTER = "__untasked__";

export type ConversationListSectionProps = {
  conversations: ConversationSummary[];
  deployments: DeploymentView[];
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onRenameConversationTitle?: (
    sessionId: string,
    title: string,
  ) => void | Promise<void>;
};

export function ConversationListSection({
  conversations,
  deployments,
  selectedAgentDid,
  selectedSessionId,
  onSelectSession,
  onRenameConversationTitle,
}: ConversationListSectionProps) {
  const [query, setQuery] = useState("");
  const [renamingSessionId, setRenamingSessionId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");

  function commitRename() {
    const title = renameDraft.trim();
    const sessionId = renamingSessionId;
    setRenamingSessionId(null);
    if (sessionId && title && onRenameConversationTitle) {
      void onRenameConversationTitle(sessionId, title);
    }
  }
  const selectedDeployment = deployments.find(
    (item) => item.agentDid === selectedAgentDid,
  );
  const selectedDeploymentLabel = selectedDeployment?.label ?? "Chat";
  const tasks = selectedDeployment?.tasks ?? [];
  const hasUntaskedConversations = conversations.some(
    (conversation) => !conversation.taskId,
  );
  const taskFilterOptions = useMemo(
    () => tasks.filter((task) => task.taskId.trim().length > 0),
    [tasks],
  );
  const [selectedTaskFilter, setSelectedTaskFilter] = useState(ALL_TASKS_FILTER);

  useEffect(() => {
    setSelectedTaskFilter(ALL_TASKS_FILTER);
    setQuery("");
    setRenamingSessionId(null);
  }, [selectedAgentDid]);

  useEffect(() => {
    if (
      selectedTaskFilter !== ALL_TASKS_FILTER &&
      selectedTaskFilter !== UNTASKED_FILTER &&
      !taskFilterOptions.some((task) => task.taskId === selectedTaskFilter)
    ) {
      setSelectedTaskFilter(ALL_TASKS_FILTER);
    }

    if (selectedTaskFilter === UNTASKED_FILTER && !hasUntaskedConversations) {
      setSelectedTaskFilter(ALL_TASKS_FILTER);
    }
  }, [hasUntaskedConversations, selectedTaskFilter, taskFilterOptions]);

  const filteredConversations = useMemo(() => {
    let rows = conversations;
    if (selectedTaskFilter === UNTASKED_FILTER) {
      rows = rows.filter((conversation) => !conversation.taskId);
    } else if (selectedTaskFilter !== ALL_TASKS_FILTER) {
      rows = rows.filter((conversation) => conversation.taskId === selectedTaskFilter);
    }
    const needle = query.trim().toLowerCase();
    if (needle) {
      rows = rows.filter((conversation) =>
        `${displayConversationTitle(conversation.title)} ${conversation.previewText ?? ""}`
          .toLowerCase()
          .includes(needle),
      );
    }
    return rows;
  }, [conversations, query, selectedTaskFilter]);
  const showTaskFilter =
    Boolean(selectedAgentDid) &&
    conversations.length > 0 &&
    (taskFilterOptions.length > 0 || hasUntaskedConversations);

  useEffect(() => {
    if (
      !selectedAgentDid ||
      !selectedSessionId ||
      selectedTaskFilter === ALL_TASKS_FILTER ||
      filteredConversations.length === 0 ||
      filteredConversations.some(
        (conversation) => conversation.sessionId === selectedSessionId,
      )
    ) {
      return;
    }

    onSelectSession(filteredConversations[0].sessionId);
  }, [
    filteredConversations,
    onSelectSession,
    selectedAgentDid,
    selectedSessionId,
    selectedTaskFilter,
  ]);

  return (
    <section className="sidebar-section conversation-section">
      <div className="panel-header">
        <div>
          <p className="eyebrow">Conversations</p>
          <h2>{selectedDeploymentLabel}</h2>
        </div>
      </div>
      {showTaskFilter ? (
        <label className="conversation-filter">
          <span>Task</span>
          <select
            data-testid="conversation-task-filter"
            onChange={(event) => setSelectedTaskFilter(event.target.value)}
            value={selectedTaskFilter}
          >
            <option value={ALL_TASKS_FILTER}>All tasks</option>
            {taskFilterOptions.map((task) => (
              <option key={task.taskId} value={task.taskId}>
                {displayTaskLabel(task)}
              </option>
            ))}
            {hasUntaskedConversations ? (
              <option value={UNTASKED_FILTER}>Manual</option>
            ) : null}
          </select>
        </label>
      ) : null}
      {selectedAgentDid && conversations.length > 0 ? (
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
      ) : !conversations.length ? (
        <p className="muted">
          No conversations yet. Sending the first message will create one automatically.
        </p>
      ) : !filteredConversations.length ? (
        <p className="muted">
          {query.trim()
            ? "No conversations match the search."
            : "No conversations for this task."}
        </p>
      ) : (
        <div className="list conversation-list">
          {filteredConversations.map((conversation) => {
            const when = conversation.updatedAt ?? conversation.createdAt;
            if (conversation.sessionId === renamingSessionId) {
              return (
                <input
                  autoFocus
                  className="conversation-rename-input"
                  data-testid={`conversation-rename-input-${conversation.sessionId}`}
                  key={conversation.sessionId}
                  onBlur={commitRename}
                  onChange={(event) => setRenameDraft(event.currentTarget.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      commitRename();
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
                  onClick={() => onSelectSession(conversation.sessionId)}
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

function displayTaskLabel(task: TaskView) {
  const name = task.name?.trim();
  return name && name.length > 0 ? name : task.taskId;
}

function displayConversationTaskLabel(conversation: ConversationSummary) {
  const name = conversation.taskName?.trim();
  return name && name.length > 0 ? name : (conversation.taskId ?? "Task");
}
