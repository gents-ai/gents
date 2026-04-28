import type { ConversationSummary, DeploymentView } from "../../lib/types";
import { displayConversationTitle } from "../../lib/types";
import { conversationStatusClass } from "./sidebarUtils";

export type ConversationListSectionProps = {
  conversations: ConversationSummary[];
  deployments: DeploymentView[];
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
};

export function ConversationListSection({
  conversations,
  deployments,
  selectedAgentDid,
  selectedSessionId,
  onSelectSession,
}: ConversationListSectionProps) {
  const selectedDeploymentLabel =
    deployments.find((item) => item.agentDid === selectedAgentDid)?.label ??
    "Chat";

  return (
    <section className="sidebar-section">
      <div className="panel-header">
        <div>
          <p className="eyebrow">Conversations</p>
          <h2>{selectedDeploymentLabel}</h2>
        </div>
      </div>
      {!selectedAgentDid ? (
        <p className="muted">Select a deployment to see conversations.</p>
      ) : !conversations.length ? (
        <p className="muted">
          No conversations yet. Sending the first message will create one
          automatically.
        </p>
      ) : (
        <div className="list">
          {conversations.map((conversation) => (
            <button
              className={
                conversation.sessionId === selectedSessionId
                  ? "list-item selected"
                  : "list-item"
              }
              data-testid={`conversation-${conversation.sessionId}`}
              key={conversation.sessionId}
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
              </span>
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
