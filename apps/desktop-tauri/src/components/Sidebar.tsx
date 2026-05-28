import type { BehaviorView, ConversationSummary, DeploymentView } from "../lib/types";
import {
  BehaviorSelectorSection,
  ConnectedPeerSection,
  ConversationListSection,
} from "./sidebar-widgets";

export type SidebarProps = {
  deployments: DeploymentView[];
  conversations: ConversationSummary[];
  behaviorOptions: BehaviorView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  onOpenFleet: () => void;
  onConfigureDeployment: (agentDid: string) => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSelectSession: (sessionId: string) => void;
  onStartNewConversation: (behaviorId: string) => void;
};

export function Sidebar({
  deployments,
  conversations,
  behaviorOptions,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionId,
  onOpenFleet,
  onConfigureDeployment,
  onSelectBehavior,
  onSelectSession,
  onStartNewConversation,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <ConnectedPeerSection
        deployments={deployments}
        selectedAgentDid={selectedAgentDid}
        onConfigureDeployment={onConfigureDeployment}
        onOpenFleet={onOpenFleet}
      />

      <BehaviorSelectorSection
        behaviorOptions={behaviorOptions}
        selectedAgentDid={selectedAgentDid}
        selectedBehaviorId={selectedBehaviorId}
        onSelectBehavior={onSelectBehavior}
        onStartNewConversation={onStartNewConversation}
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
