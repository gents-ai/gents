import { useState } from "react";

import { ChatWorkspace } from "./components/ChatWorkspace";
import { ConfigWorkspace } from "./components/ConfigWorkspace";
import { FleetDashboard } from "./components/fleet/FleetDashboard";
import { Sidebar } from "./components/Sidebar";
import { useDesktopShell } from "./hooks/useDesktopShell";
import "./App.css";

function App() {
  const shell = useDesktopShell();
  const [workspaceView, setWorkspaceView] = useState<"fleet" | "chat" | "config">(
    "fleet",
  );

  function openChat(agentDid?: string) {
    if (agentDid) {
      shell.setSelectedAgentDid(agentDid);
    }
    setWorkspaceView("chat");
  }

  function openConfig(agentDid?: string) {
    if (agentDid) {
      shell.setSelectedAgentDid(agentDid);
    }
    setWorkspaceView("config");
  }

  return (
    <main className="app-shell">
      {shell.error ? (
        <div className="callout error-banner" data-testid="error-banner">
          {shell.error}
        </div>
      ) : null}

      {workspaceView === "fleet" ? (
        <FleetDashboard
          addingPeer={shell.addingPeer}
          bootstrap={shell.snapshot?.bootstrap ?? null}
          deployments={shell.deployments}
          loading={shell.loading}
          p2pHealth={shell.runtimeHealth}
          repairingP2P={shell.repairingP2P}
          starting={shell.starting}
          onAddPeer={shell.onAddPeer}
          onOpenChat={openChat}
          onOpenConfig={openConfig}
          onRepairP2P={shell.onRepairP2P}
        />
      ) : workspaceView === "chat" ? (
        <section className="workspace">
          <Sidebar
            behaviorOptions={shell.behaviorOptions}
            conversations={shell.selectedDeployment?.conversations ?? []}
            deployments={shell.deployments}
            onConfigureDeployment={(agentDid) => openConfig(agentDid)}
            onOpenFleet={() => setWorkspaceView("fleet")}
            onSelectBehavior={shell.setSelectedBehaviorId}
            onSelectDeployment={shell.setSelectedAgentDid}
            onSelectSession={shell.setSelectedSessionId}
            selectedAgentDid={shell.selectedAgentDid}
            selectedBehaviorId={shell.selectedBehaviorId}
            selectedSessionId={shell.selectedSessionId}
          />

          <section className="chat-column">
            <ChatWorkspace
              approxSerializedBytes={shell.snapshot?.client?.approxSerializedBytes ?? 0}
              canSend={shell.canSendMessage}
              configuredPeerCount={shell.snapshot?.client?.configuredPeerCount ?? 0}
              dialedPeerCount={shell.snapshot?.client?.dialedPeerCount ?? 0}
              draft={shell.draft}
              onDraftChange={shell.setDraft}
              onRenameConversationTitle={(sessionId, title) =>
                void shell.onRenameConversationTitle(sessionId, title)
              }
              onSend={shell.onSendMessage}
              rowCount={shell.snapshot?.client?.rowCount ?? 0}
              runtimeHealth={shell.runtimeHealth}
              sendHint={
                shell.sendStatus.kind === "disabled" ? shell.sendStatus.hint : null
              }
              selectedBehaviorId={shell.selectedBehaviorId}
              selectedConversationTitle={
                shell.session
                  ? shell.session.title ?? null
                  : shell.selectedConversation?.title ?? null
              }
              selectedDeployment={shell.selectedDeployment}
              selectedSessionId={shell.selectedSessionId}
              sending={shell.sending}
              session={shell.session}
            />
          </section>
        </section>
      ) : (
        <section className="config-page">
          <ConfigWorkspace
            bootstrap={shell.snapshot?.bootstrap ?? null}
            onBack={() => setWorkspaceView("fleet")}
            onSaveAgentConfig={shell.onSaveAgentConfig}
            onRunTask={shell.onRunTask}
            onSaveBackendConfig={shell.onSaveBackendConfig}
            onSaveBehaviorConfig={shell.onSaveBehaviorConfig}
            onSaveEventTriggerConfig={shell.onSaveEventTriggerConfig}
            onSaveInferenceProfileConfig={shell.onSaveInferenceProfileConfig}
            onSaveScheduleConfig={shell.onSaveScheduleConfig}
            onSaveTaskConfig={shell.onSaveTaskConfig}
            onSaveToolSelectionConfig={shell.onSaveToolSelectionConfig}
            onSaveToolServiceConfig={shell.onSaveToolServiceConfig}
            onTestToolService={shell.onTestToolService}
            onRunSchedule={shell.onRunSchedule}
            runningTask={shell.runningTask}
            saving={shell.savingConfig}
            selectedBehaviorId={shell.selectedBehaviorId}
            selectedDeployment={shell.selectedDeployment}
          />
        </section>
      )}
    </main>
  );
}

export default App;
