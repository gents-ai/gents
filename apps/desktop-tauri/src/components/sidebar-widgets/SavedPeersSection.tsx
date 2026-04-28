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
          No saved deployments yet. Return to the fleet dashboard to add an
          agent connection.
        </p>
      ) : (
        <div className="list">
          {deployments.map((deployment) => {
            const agentIdentity = displayAgentIdentity(deployment.agentDid);
            return (
              <div className="peer-item" key={deployment.peerId}>
                <button
                  className={
                    deployment.agentDid === selectedAgentDid
                      ? "list-item selected peer-button"
                      : "list-item peer-button"
                  }
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
                  className="icon-button peer-config-button"
                  data-testid={`deployment-config-${deployment.peerId}`}
                  onClick={() => onConfigureDeployment(deployment.agentDid)}
                  title={`Configure ${deployment.label}`}
                  type="button"
                >
                  <span aria-hidden="true">&#9881;</span>
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
