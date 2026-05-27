import type { DeploymentView } from "../../lib/types";
import { displayAgentIdentity } from "../../lib/types";

export type SavedPeersSectionProps = {
  deployments: DeploymentView[];
  selectedAgentDid: string | null;
  onSelectDeployment: (agentDid: string) => void;
  onConfigureDeployment: (agentDid: string) => void;
};

export function SavedPeersSection({
  deployments,
  selectedAgentDid,
  onSelectDeployment,
  onConfigureDeployment,
}: SavedPeersSectionProps) {
  return (
    <section className="sidebar-section">
      <div className="panel-header">
        <div>
          <p className="eyebrow">Deployments</p>
          <h2>Saved Peers</h2>
        </div>
      </div>
      {!deployments.length ? (
        <p className="muted">
          No saved deployments yet. Return to the fleet dashboard to add an agent
          connection.
        </p>
      ) : (
        <div className="list">
          {deployments.map((deployment) => {
            const agentIdentity = displayAgentIdentity(deployment.agentDid);
            return (
              <div
                className={
                  deployment.agentDid === selectedAgentDid
                    ? "list-item selected peer-item-card"
                    : "list-item peer-item-card"
                }
                key={deployment.peerId}
              >
                <button
                  className="peer-select-button"
                  data-testid={`deployment-${deployment.peerId}`}
                  onClick={() => onSelectDeployment(deployment.agentDid)}
                  type="button"
                >
                  <span className="list-item-title">{deployment.label}</span>
                  <span className="list-item-meta">
                    {deployment.dialSucceeded ? "connected" : "saved"}
                  </span>
                  {agentIdentity ? (
                    <span className="list-item-subtle">{agentIdentity}</span>
                  ) : null}
                </button>
                <button
                  aria-label={`Configure ${deployment.label}`}
                  className="peer-config-button"
                  data-testid={`deployment-config-${deployment.peerId}`}
                  onClick={() => onConfigureDeployment(deployment.agentDid)}
                  title={`Configure ${deployment.label}`}
                  type="button"
                >
                  <GearIcon />
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

function GearIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.03.03a2 2 0 1 1-2.83 2.83l-.03-.03A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 .58V20a2 2 0 1 1-4 0v-.02a1.7 1.7 0 0 0-1-.58 1.7 1.7 0 0 0-1.87.34l-.03.03a2 2 0 1 1-2.83-2.83l.03-.03A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-.58-1H4a2 2 0 1 1 0-4h.02a1.7 1.7 0 0 0 .58-1 1.7 1.7 0 0 0-.34-1.87l-.03-.03A2 2 0 1 1 7.06 4.27l.03.03A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-.58V4a2 2 0 1 1 4 0v.02a1.7 1.7 0 0 0 1 .58 1.7 1.7 0 0 0 1.87-.34l.03-.03a2 2 0 1 1 2.83 2.83l-.03.03A1.7 1.7 0 0 0 19.4 9a1.7 1.7 0 0 0 .58 1H20a2 2 0 1 1 0 4h-.02a1.7 1.7 0 0 0-.58 1Z" />
    </svg>
  );
}
