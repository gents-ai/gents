import { ChatWorkspace } from "./components/ChatWorkspace";
import { Sidebar } from "./components/Sidebar";
import { useDesktopShell } from "./hooks/useDesktopShell";
import "./App.css";

function App() {
  const shell = useDesktopShell();

  return (
    <main className="app-shell">
      {shell.error ? <div className="callout error-banner">{shell.error}</div> : null}

      <section className="workspace">
        <Sidebar
          conversations={shell.selectedDeployment?.conversations ?? []}
          deployments={shell.deployments}
          dangerouslyOverwrite={shell.dangerouslyOverwrite}
          initSummary={shell.initSummary}
          initializing={shell.initializing}
          label={shell.label}
          onDangerouslyOverwriteChange={shell.setDangerouslyOverwrite}
          onInit={shell.onInit}
          onLabelChange={shell.setLabel}
          onResetChange={shell.setReset}
          onRefresh={() => void shell.refreshSnapshot()}
          onSelectDeployment={shell.setSelectedAgentDid}
          onSelectSession={shell.setSelectedSessionId}
          onShutdown={() => void shell.onShutdownClient()}
          onStart={() => void shell.onStartClient()}
          reset={shell.reset}
          running={Boolean(shell.snapshot?.client)}
          runtimeHealth={shell.runtimeHealth}
          selectedAgentDid={shell.selectedAgentDid}
          selectedSessionId={shell.selectedSessionId}
          starting={shell.starting}
          stopping={shell.stopping}
        />

        <section className="chat-column">
          <ChatWorkspace
            approxSerializedBytes={shell.snapshot?.client?.approxSerializedBytes ?? 0}
            behaviorOptions={shell.behaviorOptions}
            configuredPeerCount={shell.snapshot?.client?.configuredPeerCount ?? 0}
            dialedPeerCount={shell.snapshot?.client?.dialedPeerCount ?? 0}
            draft={shell.draft}
            onDraftChange={shell.setDraft}
            onRenameConversationTitle={(sessionId, title) =>
              void shell.onRenameConversationTitle(sessionId, title)
            }
            onSelectBehavior={shell.setSelectedBehaviorId}
            onSend={shell.onSendMessage}
            onStart={() => void shell.onStartClient()}
            rowCount={shell.snapshot?.client?.rowCount ?? 0}
            running={Boolean(shell.snapshot?.client)}
            runtimeHealth={shell.runtimeHealth}
            selectedBehaviorId={shell.selectedBehaviorId}
            selectedConversationTitle={shell.selectedConversation?.title ?? null}
            selectedDeployment={shell.selectedDeployment}
            selectedSessionId={shell.selectedSessionId}
            sending={shell.sending}
            session={shell.session}
            sessionTools={shell.sessionTools}
            starting={shell.starting}
          />
        </section>
      </section>
    </main>
  );
}

export default App;
