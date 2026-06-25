import { useState } from "react";

import type {
  BootstrapSummary,
  DeploymentView,
  P2PHealth,
  PeerAddRequest,
} from "../../lib/types";
import { formatPeerConnectionError } from "../../lib/peerConnectionErrors";
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
  onInitLocalRuntime: (label?: string | null) => Promise<unknown>;
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
  onInitLocalRuntime,
  onOpenChat,
  onOpenConfig,
  onRepairP2P,
}: FleetDashboardProps) {
  const [showAddPeer, setShowAddPeer] = useState(false);
  const [peerForm, setPeerForm] = useState(DEFAULT_PEER_FORM);
  const [peerFormError, setPeerFormError] = useState<string | null>(null);
  const [localRuntimeError, setLocalRuntimeError] = useState<string | null>(null);
  const hasDeployments = deployments.length > 0;

  async function submitPeer(request: PeerAddRequest) {
    setPeerFormError(null);
    setLocalRuntimeError(null);
    try {
      await onAddPeer({
        ...request,
        agentDid: validateAgentDid(request.agentDid),
      });
      setPeerForm(DEFAULT_PEER_FORM);
      setShowAddPeer(false);
    } catch (error) {
      setPeerFormError(formatPeerConnectionError(error, "add-peer"));
    }
  }

  async function connectLocalRuntime() {
    setLocalRuntimeError(null);
    setPeerFormError(null);
    try {
      await onInitLocalRuntime(bootstrap?.initAgentName ?? "Local Agent");
    } catch (error) {
      setLocalRuntimeError(formatPeerConnectionError(error, "local-runtime"));
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
          <LocalRuntimeConnect
            bootstrap={bootstrap}
            busy={addingPeer || starting || loading}
            error={localRuntimeError}
            onConnect={connectLocalRuntime}
          />
          <AddPeerForm
            addingPeer={addingPeer}
            disabled={starting || loading}
            localError={peerFormError}
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
          <LocalRuntimeConnect
            bootstrap={bootstrap}
            busy={addingPeer || starting || loading}
            error={localRuntimeError}
            onConnect={connectLocalRuntime}
          />
          <AddPeerForm
            addingPeer={addingPeer}
            disabled={starting || loading}
            localError={peerFormError}
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

function LocalRuntimeConnect({
  bootstrap,
  busy,
  error,
  onConnect,
}: {
  bootstrap: BootstrapSummary | null;
  busy: boolean;
  error: string | null;
  onConnect: () => Promise<void>;
}) {
  const agentName = bootstrap?.initAgentName?.trim() || "Local Agent";
  const identity = bootstrap?.initAgentDid?.trim() || bootstrap?.defaultAgentHome || "";

  return (
    <section className="fleet-local-runtime">
      <div className="fleet-local-runtime-copy">
        <span className="eyebrow">Local runtime</span>
        <strong>{agentName}</strong>
        {identity ? (
          <span className="muted mono" title={identity}>
            {identity}
          </span>
        ) : null}
      </div>
      <button
        className="primary-button"
        data-testid="fleet-connect-local"
        disabled={busy}
        onClick={() => void onConnect()}
        type="button"
      >
        {busy ? "Connecting..." : "Connect Local Agent"}
      </button>
      {error ? <p className="fleet-local-runtime-error">{error}</p> : null}
    </section>
  );
}
