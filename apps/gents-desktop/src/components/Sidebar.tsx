import { useEffect, useState } from "react";

import type {
  ConversationSummary,
  DeploymentView,
  MailboxItemView,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import {
  BehaviorEnvironmentSection,
  ConnectedPeerSection,
  ConversationListSection,
} from "./sidebar-widgets";
import { SyncHealthIndicator } from "./SyncHealthIndicator";

export type SidebarProps = {
  deployments: DeploymentView[];
  conversations: ConversationSummary[];
  mailboxItems: MailboxItemView[];
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  onOpenFleet: () => void;
  onConfigureDeployment: (agentDid: string) => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSelectSession: (sessionId: string) => void;
  onOpenSession?: (sessionId: string) => void;
  onSelectAgent?: (agentDid: string) => void;
  onStartNewConversation: (behaviorId: string) => void;
  onOpenMailboxItem: (itemId: string) => void;
  onDismissMailboxItem: (itemId: string) => void;
  onRepairP2P?: () => Promise<unknown> | void;
  repairingP2P?: boolean;
  syncHealth?: SyncHealthView | null;
};

export function Sidebar({
  deployments,
  conversations,
  mailboxItems,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionId,
  onOpenFleet,
  onConfigureDeployment,
  onSelectBehavior,
  onSelectSession,
  onOpenSession,
  onSelectAgent,
  onStartNewConversation,
  onOpenMailboxItem,
  onDismissMailboxItem,
  onRepairP2P,
  repairingP2P,
  syncHealth = null,
}: SidebarProps) {
  const [section, setSection] = useState<"sessions" | "mailbox" | "behaviors">(
    "sessions",
  );
  const selectedDeployment = deployments.find(
    (deployment) => deployment.agentDid === selectedAgentDid,
  );
  const environments = selectedDeployment?.behaviorEnvironments ?? [];

  useEffect(() => setSection("sessions"), [selectedAgentDid]);

  return (
    <aside className="sidebar">
      <ConnectedPeerSection
        deployments={deployments}
        onSelectAgent={onSelectAgent}
        selectedAgentDid={selectedAgentDid}
        onConfigureDeployment={onConfigureDeployment}
        onOpenFleet={onOpenFleet}
        onRepairP2P={onRepairP2P}
        repairingP2P={repairingP2P}
      />
      <SyncHealthIndicator syncHealth={syncHealth} />

      <div aria-label="Agent workspace" className="agent-section-tabs" role="group">
        <button
          aria-pressed={section === "mailbox"}
          className={section === "mailbox" ? "selected" : ""}
          data-testid="agent-tab-mailbox"
          onClick={() => setSection("mailbox")}
          type="button"
        >
          Mailbox{mailboxItems.length ? ` (${mailboxItems.length})` : ""}
        </button>
        <button
          aria-pressed={section === "sessions"}
          className={section === "sessions" ? "selected" : ""}
          data-testid="agent-tab-sessions"
          onClick={() => setSection("sessions")}
          type="button"
        >
          Sessions
        </button>
        <button
          aria-pressed={section === "behaviors"}
          className={section === "behaviors" ? "selected" : ""}
          data-testid="agent-tab-behaviors"
          onClick={() => setSection("behaviors")}
          type="button"
        >
          Behaviors
        </button>
      </div>

      {section === "sessions" ? (
        <ConversationListSection
          conversations={conversations}
          environments={environments}
          selectedAgentDid={selectedAgentDid}
          selectedSessionId={selectedSessionId}
          onSelectSession={onSelectSession}
          onOpenSession={onOpenSession}
          onCreateSession={() => setSection("behaviors")}
        />
      ) : section === "mailbox" ? (
        <section aria-label="Mailbox" className="sidebar-section mailbox-list">
          {mailboxItems.length === 0 ? (
            <p className="empty-state">Nothing needs your attention.</p>
          ) : (
            <div className="list" data-scroll-owner="section-list">
              {mailboxItems.map((item) => (
                <article className="list-item mailbox-item" key={item.itemId}>
                  <div className="list-item-title">{item.title}</div>
                  <div className="list-item-meta">
                    {item.kind} · {new Date(item.createdAt).toLocaleString()}
                  </div>
                  <div className="list-item-meta">
                    {item.sourceKind} · {item.sourceId}
                  </div>
                  {item.summary ? <p>{item.summary}</p> : null}
                  {item.payload ? (
                    <p className="mailbox-payload">{item.payload}</p>
                  ) : null}
                  <div className="mailbox-actions">
                    {item.action === "ack" ? (
                      item.sessionId && onOpenSession ? (
                        <button
                          onClick={() => onOpenSession(item.sessionId!)}
                          type="button"
                        >
                          Open source
                        </button>
                      ) : null
                    ) : (
                      <button
                        data-testid={`mailbox-open-${item.itemId}`}
                        onClick={() => onOpenMailboxItem(item.itemId)}
                        type="button"
                      >
                        Open compose
                      </button>
                    )}
                    <button
                      data-testid={`mailbox-dismiss-${item.itemId}`}
                      onClick={() => onDismissMailboxItem(item.itemId)}
                      type="button"
                    >
                      Dismiss
                    </button>
                  </div>
                </article>
              ))}
            </div>
          )}
        </section>
      ) : (
        <BehaviorEnvironmentSection
          environments={environments}
          selectedAgentDid={selectedAgentDid}
          selectedBehaviorId={selectedBehaviorId}
          onSelectBehavior={onSelectBehavior}
          onStartNewConversation={(behaviorId) => {
            onStartNewConversation(behaviorId);
            setSection("sessions");
          }}
        />
      )}
    </aside>
  );
}
