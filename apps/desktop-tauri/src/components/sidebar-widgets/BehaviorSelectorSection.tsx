import type { BehaviorView } from "../../lib/types";
import { boolText } from "./sidebarUtils";

export type BehaviorSelectorSectionProps = {
  behaviorOptions: BehaviorView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  onSelectBehavior: (behaviorId: string) => void;
};

export function BehaviorSelectorSection({
  behaviorOptions,
  selectedAgentDid,
  selectedBehaviorId,
  onSelectBehavior,
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
            <button
              className={
                behavior.behaviorId === selectedBehaviorId
                  ? "list-item selected"
                  : "list-item"
              }
              data-testid={`sidebar-behavior-${behavior.behaviorId}`}
              key={behavior.behaviorId}
              onClick={() => onSelectBehavior(behavior.behaviorId)}
              type="button"
            >
              <span className="list-item-title">{behavior.displayName}</span>
              <span className="list-item-meta">
                {behavior.isDefault ? "default" : boolText(behavior.enabled)}
              </span>
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
