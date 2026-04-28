import type {
  BehaviorView,
  ConversationSummary,
  DeploymentView,
} from "../lib/types";
import {
  BehaviorSelectorSection,
  ConversationListSection,
  SavedPeersSection,
} from "./sidebar-widgets";

export type SidebarProps = {
  deployments: DeploymentView[];
  conversations: ConversationSummary[];
  behaviorOptions: BehaviorView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  onOpenFleet: () => void;
  onSelectDeployment: (agentDid: string) => void;
  onConfigureDeployment: (agentDid: string) => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSelectSession: (sessionId: string) => void;
};

export function Sidebar({
  deployments,
  conversations,
  behaviorOptions,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionId,
  onOpenFleet,
  onSelectDeployment,
  onConfigureDeployment,
  onSelectBehavior,
  onSelectSession,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <section className="sidebar-section">
        <button
          className="ghost-button sidebar-wide-button sidebar-nav-button"
          onClick={onOpenFleet}
          type="button"
        >
          Fleet Dashboard
        </button>
      </section>

      <SavedPeersSection
        deployments={deployments}
        selectedAgentDid={selectedAgentDid}
        onConfigureDeployment={onConfigureDeployment}
        onSelectDeployment={onSelectDeployment}
      />

      <BehaviorSelectorSection
        behaviorOptions={behaviorOptions}
        selectedAgentDid={selectedAgentDid}
        selectedBehaviorId={selectedBehaviorId}
        onSelectBehavior={onSelectBehavior}
      />

      <ConversationListSection
        conversations={conversations}
        deployments={deployments}
        selectedAgentDid={selectedAgentDid}
        selectedSessionId={selectedSessionId}
        onSelectSession={onSelectSession}
      />
    </aside>
  );
}
