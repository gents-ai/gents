import type { DeploymentView } from "../../lib/types";
import { displayAgentIdentity, displayGraphqlEndpoint } from "../../lib/types";

export type ConnectedPeerSectionProps = {
  deployments: DeploymentView[];
  selectedAgentDid: string | null;
  onOpenFleet: () => void;
  onConfigureDeployment: (agentDid: string) => void;
  onOpenCode?: (agentDid: string) => void;
  onSelectAgent?: (agentDid: string) => void;
};

export function ConnectedPeerSection({
  deployments,
  selectedAgentDid,
  onOpenFleet,
  onConfigureDeployment,
  onOpenCode,
  onSelectAgent,
}: ConnectedPeerSectionProps) {
  const selectedDeployment =
    deployments.find((deployment) => deployment.agentDid === selectedAgentDid) ?? null;
  const agentIdentity = displayAgentIdentity(selectedDeployment?.agentDid);
  const graphqlEndpoint = displayGraphqlEndpoint(selectedDeployment?.graphql);

  return (
    <section className="sidebar-section connected-peer-section">
      <div className="connected-peer-card">
        <div className="connected-peer-header">
          <div>
            <p className="eyebrow">Connected Peer</p>
            {deployments.length > 1 && onSelectAgent ? (
              <select
                aria-label="Switch agent"
                className="connected-peer-switcher"
                data-testid="sidebar-agent-switcher"
                onChange={(event) => onSelectAgent(event.currentTarget.value)}
                value={selectedAgentDid ?? ""}
              >
                {!selectedAgentDid ? <option value="">Select an agent</option> : null}
                {deployments.map((deployment) => (
                  <option key={deployment.agentDid} value={deployment.agentDid}>
                    {deployment.agentPrincipal.displayName ?? deployment.label}
                  </option>
                ))}
              </select>
            ) : (
              <h2>{selectedDeployment?.label ?? "No peer selected"}</h2>
            )}
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
        {graphqlEndpoint ? (
          <span
            className="connected-peer-detail"
            title={`GraphQL endpoint: ${selectedDeployment?.graphql ?? ""}`}
          >
            <span className="connected-peer-detail-label">GraphQL</span>
            <span className="connected-peer-endpoint">{graphqlEndpoint}</span>
          </span>
        ) : null}

        <div className="connected-peer-actions">
          <button
            className="ghost-button connected-peer-action"
            onClick={onOpenFleet}
            type="button"
          >
            Fleet Dashboard
          </button>
          {onOpenCode ? (
            <button
              className="ghost-button connected-peer-action"
              data-testid="sidebar-open-code"
              disabled={!selectedDeployment}
              onClick={() => {
                if (selectedDeployment) {
                  onOpenCode(selectedDeployment.agentDid);
                }
              }}
              type="button"
            >
              Code
            </button>
          ) : null}
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
