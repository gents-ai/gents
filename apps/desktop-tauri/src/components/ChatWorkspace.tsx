import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";

import type { DeploymentView, DesktopSessionSnapshot, P2PHealth } from "../lib/types";
import { displayBehaviorLabel } from "../lib/types";
import {
  previewInterruptCascade,
  interruptRequest,
} from "../lib/tauri/interruptRequest";
import { BackendHealthPanel } from "./backendHealth";
import { CascadeCancelDialog } from "./cancelUx";
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
        <p className="muted">Open the fleet dashboard to choose an agent connection.</p>
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

  const [cascade, setCascade] = useState<null | { rootRequestId: string }>(null);
  const [interruptResultBanner, setInterruptResultBanner] = useState<string | null>(
    null,
  );
  const [operationsOpen, setOperationsOpen] = useState(false);
  // Set when a background-tools row asks to focus the lineage view on its
  // parent request; falls back to the session's latest request.
  const [lineageRootOverride, setLineageRootOverride] = useState<string | null>(null);

  useEffect(() => {
    setLineageRootOverride(null);
  }, [selectedSessionId]);

  useEffect(() => {
    if (!interruptResultBanner) return;
    const t = setTimeout(() => setInterruptResultBanner(null), 5000);
    return () => clearTimeout(t);
  }, [interruptResultBanner]);

  const beginInterrupt = useCallback(
    async (requestId: string) => {
      try {
        const preview = await previewInterruptCascade({
          requestId,
          agentDid: selectedDeployment.agentDid,
          includeTerminal: false,
        });
        const childCount =
          preview.willInterrupt.length +
          preview.willDetach.length +
          preview.unknownPolicy.length;
        if (childCount === 0) {
          const result = await interruptRequest({
            requestId,
            cause: "userCancelled",
            cascade: false,
          });
          if (result.accepted) setInterruptResultBanner("Interrupt accepted");
          else if (result.alreadyInterrupted)
            setInterruptResultBanner("Already interrupted by another caller");
          return;
        }
        setCascade({ rootRequestId: requestId });
      } catch (e) {
        setInterruptResultBanner(`Interrupt preview failed: ${String(e)}`);
      }
    },
    [selectedDeployment.agentDid],
  );

  const operationsRailTabs = useMemo<OperationsRailTabDescriptor[]>(() => {
    const rootRequestId = lineageRootOverride ?? session?.latestRequestId ?? null;
    const lineageAgentDid = selectedDeployment.agentDid;
    return [
      {
        id: "background-tools",
        label: "Background",
        render: () => (
          <BackgroundedToolsPanel
            onOpenLineage={setLineageRootOverride}
            onInterruptParent={(requestId) => {
              void beginInterrupt(requestId);
            }}
          />
        ),
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
  }, [
    session?.latestRequestId,
    selectedDeployment.agentDid,
    lineageRootOverride,
    beginInterrupt,
  ]);

  function onInterruptClick() {
    const requestId = session?.latestRequestId;
    if (!requestId) return;
    void beginInterrupt(requestId);
  }

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

          {interruptResultBanner ? (
            <div
              className="muted small"
              role="status"
              aria-live="polite"
              style={{ padding: "4px 12px" }}
            >
              {interruptResultBanner}
            </div>
          ) : null}

          <ChatComposer
            activeRequestId={session?.latestRequestId ?? null}
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
            onInterruptClick={onInterruptClick}
            onSend={onSend}
          />
        </div>
        <OperationsRail open={operationsOpen} onOpenChange={setOperationsOpen} />
      </section>

      {cascade ? (
        <CascadeCancelDialog
          open
          rootRequestId={cascade.rootRequestId}
          agentDid={selectedDeployment.agentDid}
          onClose={() => setCascade(null)}
          onAccepted={(at) => {
            setCascade(null);
            setInterruptResultBanner(`Interrupt accepted at ${at ?? "(unknown)"}`);
          }}
          onAlreadyInterrupted={() => {
            setCascade(null);
            setInterruptResultBanner("Already interrupted by another caller");
          }}
          onError={(msg) => {
            setCascade(null);
            setInterruptResultBanner(`Interrupt failed: ${msg}`);
          }}
        />
      ) : null}
    </OperationsRailProvider>
  );
}
