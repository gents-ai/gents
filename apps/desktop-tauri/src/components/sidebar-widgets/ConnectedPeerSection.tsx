import type { DeploymentView } from "../../lib/types";
import { displayAgentIdentity } from "../../lib/types";

export type ConnectedPeerSectionProps = {
  deployments: DeploymentView[];
  selectedAgentDid: string | null;
  onOpenFleet: () => void;
  onConfigureDeployment: (agentDid: string) => void;
};

export function ConnectedPeerSection({
  deployments,
  selectedAgentDid,
  onOpenFleet,
  onConfigureDeployment,
}: ConnectedPeerSectionProps) {
  const selectedDeployment =
    deployments.find((deployment) => deployment.agentDid === selectedAgentDid) ??
    null;
  const agentIdentity = displayAgentIdentity(selectedDeployment?.agentDid);

  return (
    <section className="sidebar-section connected-peer-section">
      <div className="connected-peer-card">
        <div className="connected-peer-header">
          <div>
            <p className="eyebrow">Connected Peer</p>
            <h2>{selectedDeployment?.label ?? "No peer selected"}</h2>
          </div>
          {selectedDeployment ? (
            <span className="connected-peer-status">
              {selectedDeployment.dialSucceeded ? "connected" : "saved"}
            </span>
          ) : null}
        </div>

        {agentIdentity ? (
          <span className="connected-peer-identity">{agentIdentity}</span>
        ) : null}
        {selectedDeployment?.graphql ? (
          <span className="connected-peer-endpoint">{selectedDeployment.graphql}</span>
        ) : null}

        <div className="connected-peer-actions">
          <button
            className="ghost-button connected-peer-action"
            onClick={onOpenFleet}
            type="button"
          >
            Fleet Dashboard
          </button>
          <button
            className="ghost-button connected-peer-action"
            disabled={!selectedDeployment}
            onClick={() => {
              if (selectedDeployment) {
                onConfigureDeployment(selectedDeployment.agentDid);
              }
            }}
            type="button"
          >
            Configure
          </button>
        </div>
      </div>
    </section>
  );
}
