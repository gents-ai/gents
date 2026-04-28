import type { FormEvent } from "react";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
  P2PHealth,
} from "../lib/types";
import { displayBehaviorLabel } from "../lib/types";
import { ChatComposer, ChatHeader, ChatTranscriptPanel } from "./chat";

export type ChatWorkspaceProps = {
  running: boolean;
  starting: boolean;
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
  onStart: () => void;
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
  const { running, starting, selectedDeployment, onStart } = props;

  if (!running) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Chat</p>
        <h2>Start the desktop core</h2>
        <p className="lede compact">
          The Tauri shell now knows how to initialize the existing desktop
          runtime and hold a live Rust client core. Start it to debug the
          initial chat screen.
        </p>
        <button className="primary-button" disabled={starting} onClick={onStart}>
          {starting ? "Starting…" : "Start Desktop Core"}
        </button>
      </article>
    );
  }

  if (!selectedDeployment) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Chat</p>
        <h2>Select a deployment</h2>
        <p className="muted">
          Pick a saved deployment from the left rail to debug the first chat
          screen.
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

  return (
    <>
      <ChatHeader
        agentDid={selectedDeployment.agentDid}
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
      </section>
    </>
  );
}
