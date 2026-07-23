import { useState } from "react";

import type {
  BackendSaveRequest,
  BehaviorSaveRequest,
  BootstrapSummary,
  CodexLoginResult,
  DeploymentView,
  InferenceProbeResult,
  P2PHealth,
  PeerAddRequest,
} from "../../lib/types";
import { formatPeerConnectionError } from "../../lib/peerConnectionErrors";
import { AddPeerForm } from "./AddPeerForm";
import { BrandLockup } from "./BrandLockup";
import { InferenceSetupWizard } from "./InferenceSetupWizard";
import { ThemeToggle } from "../ThemeToggle";
import { FleetRow } from "./FleetRow";
import { NetworkPanel } from "./NetworkPanel";
import { validateAgentDid } from "./peerConnectionImport";

/** A deployment lacks a usable inference backend — the onboarding moment where
 *  the guided setup is worth surfacing. */
function needsInferenceSetup(deployment: DeploymentView): boolean {
  const behavior =
    deployment.behaviors.find((entry) => entry.isDefault) ?? deployment.behaviors[0];
  const backend =
    deployment.inferenceBackends.find(
      (entry) => entry.backendId === behavior?.backendId,
    ) ?? deployment.inferenceBackends[0];
  if (!backend) return true;
  if (backend.enabled === false) return true;
  return backend.models.length === 0;
}

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
  onOpenCode?: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
  onRemovePeer?: (peerId: string) => Promise<unknown> | void;
  onRenamePeer?: (peerId: string, label: string) => Promise<unknown> | void;
  onRepairP2P: () => Promise<unknown>;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onProbeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  onCodexLogin: (agentDid: string) => Promise<CodexLoginResult>;
  onCancelCodexLogin: () => Promise<unknown>;
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
  onOpenCode,
  onOpenConfig,
  onRemovePeer,
  onRenamePeer,
  onRepairP2P,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
  onProbeInferenceEndpoint,
  onCodexLogin,
  onCancelCodexLogin,
}: FleetDashboardProps) {
  const [showAddPeer, setShowAddPeer] = useState(false);
  const [peerForm, setPeerForm] = useState(DEFAULT_PEER_FORM);
  const [peerFormError, setPeerFormError] = useState<string | null>(null);
  const [localRuntimeError, setLocalRuntimeError] = useState<string | null>(null);
  const [wizardDeployment, setWizardDeployment] = useState<DeploymentView | null>(null);
  const hasDeployments = deployments.length > 0;
  const deploymentNeedingInference = deployments.find(needsInferenceSetup) ?? null;
  // Keep the open wizard bound to the freshest copy of its deployment so a
  // background snapshot refresh (post-save) flows into it.
  const activeWizardDeployment = wizardDeployment
    ? (deployments.find((entry) => entry.peerId === wizardDeployment.peerId) ??
      wizardDeployment)
    : null;
  // The repair command re-dials the desktop client's P2P connections as a
  // whole, so it lives here at fleet level — a per-row placement would lie
  // about its scope. Shown only when something actually needs repairing.
  const needsP2PRepair =
    deployments.some(
      (deployment) => !deployment.dialSucceeded || Boolean(deployment.lastError),
    ) ||
    (p2pHealth
      ? p2pHealth.consecutiveFailures > 0 || Boolean(p2pHealth.lastError)
      : false);

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
            <h2>Connect your agent</h2>
            <p className="muted">
              Start with the agent on this machine — one click, no configuration.
            </p>
          </div>
          <LocalRuntimeConnect
            bootstrap={bootstrap}
            busy={addingPeer || starting || loading}
            error={localRuntimeError}
            onConnect={connectLocalRuntime}
          />
          {/* Remote connection needs DIDs/multiaddrs — keep it behind a
              disclosure so first-run isn't a wall of connection JSON. */}
          <details
            className="fleet-remote-disclosure"
            data-testid="fleet-remote-disclosure"
          >
            <summary>Connect a remote agent instead…</summary>
            <AddPeerForm
              addingPeer={addingPeer}
              disabled={starting || loading}
              localError={peerFormError}
              peerForm={peerForm}
              onPeerFormChange={setPeerForm}
              onFetchPeerStatus={onFetchPeerStatus}
              onSubmit={submitPeer}
            />
          </details>
        </div>
      </section>
    );
  }

  return (
    <section className="fleet-dashboard" data-testid="fleet-dashboard">
      <header className="fleet-header">
        <BrandLockup />
        <div className="fleet-header-actions">
          <ThemeToggle />
          {needsP2PRepair ? (
            <button
              className="ghost-button"
              data-testid="fleet-repair-p2p"
              disabled={repairingP2P}
              onClick={() => void onRepairP2P()}
              title="Re-dial saved peers and refresh the desktop client's P2P connections"
              type="button"
            >
              {repairingP2P ? "Reconnecting…" : "Reconnect P2P"}
            </button>
          ) : null}
          <button
            className="primary-button"
            onClick={() => setShowAddPeer((value) => !value)}
            type="button"
          >
            Add Agent
          </button>
        </div>
      </header>

      {deploymentNeedingInference ? (
        <section
          className="panel fleet-inference-callout"
          data-testid="fleet-inference-callout"
        >
          <div className="fleet-inference-callout-copy">
            <span className="eyebrow">Inference</span>
            <strong>Finish setting up {deploymentNeedingInference.label}</strong>
            <span className="muted">
              This agent has no model backend yet. Connect OpenAI, a local server, a
              custom endpoint, or your ChatGPT subscription.
            </span>
          </div>
          <button
            className="primary-button"
            data-testid="fleet-inference-setup"
            type="button"
            onClick={() => setWizardDeployment(deploymentNeedingInference)}
          >
            Set up inference
          </button>
        </section>
      ) : null}

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
                onOpenChat={onOpenChat}
                onOpenCode={onOpenCode}
                onOpenConfig={onOpenConfig}
                onRemovePeer={onRemovePeer}
                onRenamePeer={onRenamePeer}
              />
            ))}
          </tbody>
        </table>
      </div>

      <NetworkPanel />

      {activeWizardDeployment ? (
        <InferenceSetupWizard
          deployment={activeWizardDeployment}
          onClose={() => setWizardDeployment(null)}
          onSaveBackendConfig={onSaveBackendConfig}
          onSaveBehaviorConfig={onSaveBehaviorConfig}
          onProbeInferenceEndpoint={onProbeInferenceEndpoint}
          onCodexLogin={onCodexLogin}
          onCancelCodexLogin={onCancelCodexLogin}
        />
      ) : null}
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
