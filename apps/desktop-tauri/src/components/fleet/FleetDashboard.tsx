import { useState } from "react";

import type {
  BootstrapSummary,
  DeploymentView,
  P2PHealth,
  PeerAddRequest,
} from "../../lib/types";
import { AddPeerForm } from "./AddPeerForm";
import { BrandLockup } from "./BrandLockup";
import { FleetRow } from "./FleetRow";
import { validateAgentDid } from "./peerConnectionImport";

type FleetDashboardProps = {
  addingPeer: boolean;
  bootstrap: BootstrapSummary | null;
  deployments: DeploymentView[];
  loading: boolean;
  p2pHealth: P2PHealth | null;
  repairingP2P: boolean;
  starting: boolean;
  onAddPeer: (request: PeerAddRequest) => Promise<unknown>;
  onFetchPeerStatus: (serverAddress: string) => Promise<unknown>;
  onOpenChat: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
  onRepairP2P: () => Promise<unknown>;
};

const DEFAULT_PEER_FORM: PeerAddRequest = {
  label: "",
  agentDid: "",
  addr: "",
  graphql: null,
};

export function FleetDashboard({
  addingPeer,
  bootstrap,
  deployments,
  loading,
  p2pHealth,
  repairingP2P,
  starting,
  onAddPeer,
  onFetchPeerStatus,
  onOpenChat,
  onOpenConfig,
  onRepairP2P,
}: FleetDashboardProps) {
  const [showAddPeer, setShowAddPeer] = useState(false);
  const [peerForm, setPeerForm] = useState(DEFAULT_PEER_FORM);
  const [localError, setLocalError] = useState<string | null>(null);
  const hasDeployments = deployments.length > 0;

  async function submitPeer(request: PeerAddRequest) {
    setLocalError(null);
    try {
      await onAddPeer({
        ...request,
        agentDid: validateAgentDid(request.agentDid),
      });
      setPeerForm(DEFAULT_PEER_FORM);
      setShowAddPeer(false);
    } catch (error) {
      setLocalError(String(error));
    }
  }

  if (!hasDeployments) {
    return (
      <section className="fleet-empty" data-testid="fleet-empty">
        <div className="fleet-empty-card panel">
          <BrandLockup />
          <div className="fleet-empty-copy">
            <h2>Add Agent Connection</h2>
            <p className="muted">
              Connect the desktop to an agent before opening chat or config.
            </p>
          </div>
          <AddPeerForm
            addingPeer={addingPeer}
            disabled={starting || loading}
            localError={localError}
            peerForm={peerForm}
            onPeerFormChange={setPeerForm}
            onFetchPeerStatus={onFetchPeerStatus}
            onSubmit={submitPeer}
          />
        </div>
      </section>
    );
  }

  return (
    <section className="fleet-dashboard" data-testid="fleet-dashboard">
      <header className="fleet-header">
        <BrandLockup />
        <div className="fleet-header-actions">
          <button
            className="primary-button"
            onClick={() => setShowAddPeer((value) => !value)}
            type="button"
          >
            Add Agent
          </button>
        </div>
      </header>

      {showAddPeer ? (
        <section className="panel fleet-add-panel">
          <AddPeerForm
            addingPeer={addingPeer}
            disabled={starting || loading}
            localError={localError}
            peerForm={peerForm}
            onPeerFormChange={setPeerForm}
            onFetchPeerStatus={onFetchPeerStatus}
            onSubmit={submitPeer}
          />
        </section>
      ) : null}

      <div className="fleet-table-wrap">
        <table className="fleet-table">
          <thead>
            <tr>
              <th>Agent</th>
              <th>Behaviors</th>
              <th>Tasks</th>
              <th>Inference</th>
              <th>Tool ceiling</th>
              <th>Open work</th>
              <th>Last update</th>
              <th className="fleet-actions-header" aria-label="Actions" />
            </tr>
          </thead>
          <tbody>
            {deployments.map((deployment) => (
              <FleetRow
                bootstrap={bootstrap}
                deployment={deployment}
                key={deployment.peerId}
                p2pHealth={p2pHealth}
                repairingP2P={repairingP2P}
                onOpenChat={onOpenChat}
                onOpenConfig={onOpenConfig}
                onRepairP2P={onRepairP2P}
              />
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
