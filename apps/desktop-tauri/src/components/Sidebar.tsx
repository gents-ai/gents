import type { FormEvent } from "react";

import type {
  BehaviorView,
  ConversationSummary,
  DeploymentView,
  InitSummary,
  P2PHealth,
} from "../lib/types";
import {
  BehaviorSelectorSection,
  ConversationListSection,
  RuntimeSetupSection,
  SavedPeersSection,
} from "./sidebar-widgets";

export type SidebarProps = {
  running: boolean;
  starting: boolean;
  stopping: boolean;
  runtimeHealth: P2PHealth | null;
  deployments: DeploymentView[];
  conversations: ConversationSummary[];
  behaviorOptions: BehaviorView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  label: string;
  dangerouslyOverwrite: boolean;
  reset: boolean;
  initializing: boolean;
  initSummary: InitSummary | null;
  onLabelChange: (value: string) => void;
  onDangerouslyOverwriteChange: (value: boolean) => void;
  onResetChange: (value: boolean) => void;
  onRefresh: () => void;
  onOpenFleet: () => void;
  onSelectDeployment: (agentDid: string) => void;
  onConfigureDeployment: (agentDid: string) => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSelectSession: (sessionId: string) => void;
  onShutdown: () => void;
  onStart: () => void;
  onInit: (event: FormEvent) => void;
};

export function Sidebar({
  running,
  starting,
  stopping,
  runtimeHealth,
  deployments,
  conversations,
  behaviorOptions,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionId,
  label,
  dangerouslyOverwrite,
  reset,
  initializing,
  initSummary,
  onLabelChange,
  onDangerouslyOverwriteChange,
  onResetChange,
  onRefresh,
  onOpenFleet,
  onSelectDeployment,
  onConfigureDeployment,
  onSelectBehavior,
  onSelectSession,
  onShutdown,
  onStart,
  onInit,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <section className="sidebar-section">
        <button
          className="ghost-button sidebar-wide-button"
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

      <RuntimeSetupSection
        dangerouslyOverwrite={dangerouslyOverwrite}
        initSummary={initSummary}
        initializing={initializing}
        label={label}
        reset={reset}
        running={running}
        runtimeHealth={runtimeHealth}
        starting={starting}
        stopping={stopping}
        onDangerouslyOverwriteChange={onDangerouslyOverwriteChange}
        onInit={onInit}
        onLabelChange={onLabelChange}
        onRefresh={onRefresh}
        onResetChange={onResetChange}
        onShutdown={onShutdown}
        onStart={onStart}
      />
    </aside>
  );
}
