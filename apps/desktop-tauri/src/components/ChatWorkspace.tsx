import { useMemo, type FormEvent } from "react";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
  P2PHealth,
} from "../lib/types";
import { displayBehaviorLabel } from "../lib/types";
import { BackendHealthPanel } from "./backendHealth";
import { ChatComposer, ChatHeader, ChatTranscriptPanel } from "./chat";
import { McpHealthPanel } from "./mcpHealth";
import { OperationsRail, OperationsRailProvider } from "./operations";
import type { OperationsRailTabDescriptor } from "./operations";
import { BackgroundedToolsPanel } from "./backgroundedTools";
import { SubagentLineageView } from "./subagentLineage";

export type ChatWorkspaceProps = {
  selectedDeployment: DeploymentView | null;
  selectedConversationTitle: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  session: DesktopSessionSnapshot | null;
  runtimeHealth: P2PHealth | null;
  rowCount: number;
  approxSerializedBytes: number;
  dialedPeerCount: number;
  configuredPeerCount: number;
  canSend: boolean;
  sendHint: string | null;
  draft: string;
  sending: boolean;
  onRenameConversationTitle: (sessionId: string, title: string) => void | Promise<void>;
  onDraftChange: (value: string) => void;
  onSend: (event: FormEvent) => void;
};

export type ActiveChatWorkspaceProps = Omit<
  ChatWorkspaceProps,
  "selectedDeployment"
> & {
  selectedDeployment: DeploymentView;
};

export function ChatWorkspace(props: ChatWorkspaceProps) {
  const { selectedDeployment } = props;

  if (!selectedDeployment) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Chat</p>
        <h2>Select an agent</h2>
        <p className="muted">
          Open the fleet dashboard to choose an agent connection.
        </p>
      </article>
    );
  }

  return <ActiveChatWorkspace {...props} selectedDeployment={selectedDeployment} />;
}

export function ActiveChatWorkspace({
  selectedDeployment,
  selectedConversationTitle,
  selectedBehaviorId,
  selectedSessionId,
  session,
  runtimeHealth,
  rowCount,
  approxSerializedBytes,
  dialedPeerCount,
  configuredPeerCount,
  canSend,
  sendHint,
  draft,
  sending,
  onRenameConversationTitle,
  onDraftChange,
  onSend,
}: ActiveChatWorkspaceProps) {
  const activeBehaviorId =
    selectedBehaviorId ?? selectedDeployment.defaultBehaviorId ?? null;
  const behaviorLabel =
    selectedDeployment.behaviors.find(
      (behavior) => behavior.behaviorId === activeBehaviorId,
    )?.displayName ?? displayBehaviorLabel(activeBehaviorId);

  const operationsRailTabs = useMemo<OperationsRailTabDescriptor[]>(() => {
    const rootRequestId = session?.latestRequestId ?? null;
    const lineageAgentDid = selectedDeployment.agentDid;
    return [
      {
        id: "background-tools",
        label: "Background",
        render: () => <BackgroundedToolsPanel />,
      },
      {
        id: "lineage",
        label: "Lineage",
        render: () => (
          <SubagentLineageView
            rootRequestId={rootRequestId}
            agentDid={lineageAgentDid}
          />
        ),
      },
      {
        id: "backend-health",
        label: "Backends",
        render: () => <BackendHealthPanel />,
      },
      {
        id: "mcp-health",
        label: "MCP health",
        render: () => <McpHealthPanel />,
      },
    ];
  }, [session?.latestRequestId, selectedDeployment.agentDid]);

  return (
    <OperationsRailProvider tabs={operationsRailTabs}>
      <ChatHeader
        behaviorLabel={behaviorLabel}
        runtimeHealth={runtimeHealth}
        selectedConversationTitle={selectedConversationTitle}
        selectedSessionId={selectedSessionId}
        onRenameConversationTitle={onRenameConversationTitle}
      />

      <section className="chat-workspace">
        <div className="chat-main">
          <ChatTranscriptPanel
            selectedSessionId={selectedSessionId}
            session={session}
          />

          <ChatComposer
            approxSerializedBytes={approxSerializedBytes}
            behaviorLabel={behaviorLabel}
            canSend={canSend}
            configuredPeerCount={configuredPeerCount}
            dialedPeerCount={dialedPeerCount}
            draft={draft}
            rowCount={rowCount}
            sendHint={sendHint}
            sending={sending}
            turnState={session?.turnState ?? null}
            onDraftChange={onDraftChange}
            onSend={onSend}
          />
        </div>
        <OperationsRail />
      </section>
    </OperationsRailProvider>
  );
}
