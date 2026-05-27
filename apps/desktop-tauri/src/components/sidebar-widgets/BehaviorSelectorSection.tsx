import type { BehaviorView } from "../../lib/types";
import { boolText } from "./sidebarUtils";

export type BehaviorSelectorSectionProps = {
  behaviorOptions: BehaviorView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  onSelectBehavior: (behaviorId: string) => void;
  onStartNewConversation: (behaviorId: string) => void;
};

export function BehaviorSelectorSection({
  behaviorOptions,
  selectedAgentDid,
  selectedBehaviorId,
  onSelectBehavior,
  onStartNewConversation,
}: BehaviorSelectorSectionProps) {
  return (
    <section className="sidebar-section">
      <div className="panel-header">
        <div>
          <p className="eyebrow">Chat</p>
          <h2>Behavior</h2>
        </div>
      </div>
      {!selectedAgentDid ? (
        <p className="muted">Select a deployment to choose behavior.</p>
      ) : !behaviorOptions.length ? (
        <p className="muted">No behaviors are available for this peer.</p>
      ) : (
        <div className="list compact-list">
          {behaviorOptions.map((behavior) => (
            <div className="behavior-list-row" key={behavior.behaviorId}>
              <button
                className={
                  behavior.behaviorId === selectedBehaviorId
                    ? "list-item behavior-select-button selected"
                    : "list-item behavior-select-button"
                }
                data-testid={`sidebar-behavior-${behavior.behaviorId}`}
                onClick={() => onSelectBehavior(behavior.behaviorId)}
                type="button"
              >
                <span className="list-item-title">{behavior.displayName}</span>
                <span className="list-item-meta">
                  {behavior.isDefault ? "default" : boolText(behavior.enabled)}
                </span>
              </button>
              <button
                aria-label={`Start new chat with ${behavior.displayName}`}
                className="behavior-new-chat-button"
                data-testid={`sidebar-new-chat-${behavior.behaviorId}`}
                onClick={() => onStartNewConversation(behavior.behaviorId)}
                title={`Start new chat with ${behavior.displayName}`}
                type="button"
              >
                <ChatPlusIcon />
              </button>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function ChatPlusIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z" />
      <path d="M12 8v6" />
      <path d="M9 11h6" />
    </svg>
  );
}
