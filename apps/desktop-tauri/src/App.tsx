import { useEffect, useState } from "react";

import { ChatWorkspace } from "./components/ChatWorkspace";
import { CodeContextHeader } from "./components/code/CodeContextHeader";
import { ConfigWorkspace } from "./components/ConfigWorkspace";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { FleetDashboard } from "./components/fleet/FleetDashboard";
import { ErrorBanner } from "./components/ErrorBanner";
import { applyTheme, loadTheme } from "./lib/theme";
import { ShortcutsHelp } from "./components/ShortcutsHelp";
import { useAppShortcuts } from "./hooks/useAppShortcuts";
import { Sidebar } from "./components/Sidebar";
import { useDesktopShell } from "./hooks/useDesktopShell";
import { installExternalLinkGuard } from "./lib/externalLinks";
import "./App.css";

function App() {
  return (
    <ErrorBoundary>
      <AppShell />
    </ErrorBoundary>
  );
}

function AppShell() {
  const shell = useDesktopShell();

  // Boot lives here, not in App: the boundary test inspects App() as a
  // plain hook-free function, and the e2e harness mounts App directly.
  useEffect(() => {
    applyTheme(loadTheme());
  }, []);

  // External links (e.g. markdown links in the transcript) must open in the
  // OS browser — an unguarded anchor click navigates the whole webview away.
  useEffect(() => installExternalLinkGuard(document), []);
  const [workspaceView, setWorkspaceView] = useState<
    "fleet" | "chat" | "config" | "code"
  >("fleet");
  const [shortcutsOpen, setShortcutsOpen] = useState(false);

  useAppShortcuts({
    setView: setWorkspaceView,
    newConversation: () => {
      const behaviorId =
        shell.selectedBehaviorId ?? shell.behaviorOptions[0]?.behaviorId ?? null;
      if (behaviorId) {
        setWorkspaceView("chat");
        shell.onStartNewConversation(behaviorId);
      }
    },
    focusComposer: () => {
      setWorkspaceView("chat");
      requestAnimationFrame(() => {
        document
          .querySelector<HTMLTextAreaElement>('[data-testid="composer-input"]')
          ?.focus();
      });
    },
    toggleHelp: () => setShortcutsOpen((open) => !open),
  });

  function openChat(agentDid?: string) {
    if (agentDid) {
      shell.setSelectedAgentDid(agentDid);
    }
    setWorkspaceView("chat");
  }

  function openCode(agentDid?: string) {
    if (agentDid) {
      shell.setSelectedAgentDid(agentDid);
    }
    setWorkspaceView("code");
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
        <ErrorBanner message={shell.error} onDismiss={shell.onDismissError} />
      ) : null}
      <ShortcutsHelp open={shortcutsOpen} onClose={() => setShortcutsOpen(false)} />

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
          onFetchPeerStatus={shell.onFetchPeerStatus}
          onInitLocalRuntime={shell.onInitLocalRuntime}
          onOpenChat={openChat}
          onOpenCode={openCode}
          onOpenConfig={openConfig}
          onRemovePeer={shell.onRemovePeer}
          onRenamePeer={shell.onRenamePeer}
          onRepairP2P={shell.onRepairP2P}
        />
      ) : workspaceView === "chat" || workspaceView === "code" ? (
        <section className="workspace">
          <Sidebar
            behaviorOptions={shell.behaviorOptions}
            conversations={shell.selectedDeployment?.conversations ?? []}
            deployments={shell.deployments}
            onConfigureDeployment={(agentDid) => openConfig(agentDid)}
            onOpenCode={(agentDid) => openCode(agentDid)}
            onOpenFleet={() => setWorkspaceView("fleet")}
            onSelectBehavior={shell.setSelectedBehaviorId}
            onSelectAgent={(agentDid) => {
              shell.setSelectedAgentDid(agentDid);
              shell.setSelectedSessionId(null);
            }}
            onSelectSession={shell.onSelectSession}
            onRenameConversationTitle={shell.onRenameConversationTitle}
            onStartNewConversation={shell.onStartNewConversation}
            selectedAgentDid={shell.selectedAgentDid}
            selectedBehaviorId={shell.selectedBehaviorId}
            selectedSessionId={shell.selectedSessionId}
          />

          <section className="chat-column">
            {workspaceView === "code" ? (
              <CodeContextHeader
                deployment={shell.selectedDeployment ?? null}
                selectedBehaviorId={shell.selectedBehaviorId}
                onBackToChat={() => setWorkspaceView("chat")}
              />
            ) : null}
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
              onRetryMessage={shell.onRetryMessage}
              rowCount={shell.snapshot?.client?.rowCount ?? 0}
              runtimeHealth={shell.runtimeHealth}
              sendHint={
                shell.sendStatus.kind === "disabled" ? shell.sendStatus.hint : null
              }
              selectedBehaviorId={shell.selectedBehaviorId}
              selectedConversationTitle={
                shell.session
                  ? (shell.session.title ?? null)
                  : (shell.selectedConversation?.title ?? null)
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
            onBack={() => setWorkspaceView("chat")}
            onDeleteSkillConfig={shell.onDeleteSkillConfig}
            onDeleteTaskConfig={shell.onDeleteTaskConfig}
            onDeleteScheduleConfig={shell.onDeleteScheduleConfig}
            onDeleteEventTriggerConfig={shell.onDeleteEventTriggerConfig}
            onSaveAgentConfig={shell.onSaveAgentConfig}
            onRunTask={shell.onRunTask}
            onSaveBackendConfig={shell.onSaveBackendConfig}
            onSaveBehaviorConfig={shell.onSaveBehaviorConfig}
            onSaveEventTriggerConfig={shell.onSaveEventTriggerConfig}
            onSaveInferenceProfileConfig={shell.onSaveInferenceProfileConfig}
            onSaveScheduleConfig={shell.onSaveScheduleConfig}
            onSaveSkillConfig={shell.onSaveSkillConfig}
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
