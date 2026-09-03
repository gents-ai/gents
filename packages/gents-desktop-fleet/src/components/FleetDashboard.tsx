import { useEffect, useRef, useState, type ReactNode } from "react";

import type {
  BootstrapSummary,
  DesktopApiAdapter,
  DeploymentView,
  EnrollmentRequestView,
  P2PHealth,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import type { FleetCopy } from "../copy.js";
import { needsInferenceSetup } from "../fleetMetrics.js";
import { AddPeerForm, type AddPeerFormProps } from "./AddPeerForm.js";
import { FleetRow } from "./FleetRow.js";
import { NetworkPanel } from "./NetworkPanel.js";

export type FleetDashboardProps = {
  addingPeer: boolean;
  bootstrap: BootstrapSummary | null;
  deployments: DeploymentView[];
  enrollmentRequests: EnrollmentRequestView[] | null;
  loading: boolean;
  p2pHealth: P2PHealth | null;
  syncHealth?: SyncHealthView | null;
  repairingP2P: boolean;
  starting: boolean;
  onRequestStatusEnrollment: AddPeerFormProps["onRequestStatusEnrollment"];
  onOpenChat: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
  onRemovePeer?: (peerId: string) => Promise<unknown> | void;
  onRenamePeer?: (peerId: string, label: string) => Promise<unknown> | void;
  onRepairP2P: () => Promise<unknown>;
  brand?: ReactNode;
  api: DesktopApiAdapter;
  copy?: FleetCopy;
  headerLeadingActions?: ReactNode;
  localRuntimeSetup?: ReactNode;
  renderInferenceSetup?: (
    deployment: DeploymentView,
    close: () => void,
  ) => ReactNode;
};

export function FleetDashboard({
  addingPeer,
  bootstrap,
  deployments,
  enrollmentRequests,
  p2pHealth,
  syncHealth = null,
  repairingP2P,
  starting,
  onRequestStatusEnrollment,
  onOpenChat,
  onOpenConfig,
  onRemovePeer,
  onRenamePeer,
  onRepairP2P,
  brand,
  api,
  copy,
  headerLeadingActions,
  localRuntimeSetup,
  renderInferenceSetup,
}: FleetDashboardProps) {
  const [showAddPeer, setShowAddPeer] = useState(false);
  const [wizardDeployment, setWizardDeployment] =
    useState<DeploymentView | null>(null);
  const autoPromptedInference = useRef(new Set<string>());
  const hasDeployments = deployments.length > 0;
  const deploymentNeedingInference =
    deployments.find(needsInferenceSetup) ?? null;
  const activeWizardDeployment = wizardDeployment
    ? (deployments.find((entry) => entry.peerId === wizardDeployment.peerId) ??
      wizardDeployment)
    : null;
  const needsP2PRepair =
    deployments.some(
      (deployment) =>
        !deployment.dialSucceeded || Boolean(deployment.lastError),
    ) ||
    (p2pHealth
      ? p2pHealth.consecutiveFailures > 0 || Boolean(p2pHealth.lastError)
      : false);

  useEffect(() => {
    if (
      !renderInferenceSetup ||
      !deploymentNeedingInference ||
      deploymentNeedingInference.source !== "local-standard" ||
      autoPromptedInference.current.has(deploymentNeedingInference.peerId)
    ) {
      return;
    }
    autoPromptedInference.current.add(deploymentNeedingInference.peerId);
    setWizardDeployment(deploymentNeedingInference);
  }, [deploymentNeedingInference, renderInferenceSetup]);

  async function requestStatusEnrollment(serverAddress: string) {
    const request = await onRequestStatusEnrollment(serverAddress);
    setShowAddPeer(false);
    return request;
  }

  const enrollmentNotice = enrollmentRequests === null ? (
    <section
      aria-live="polite"
      className="fleet-enrollment-pending"
      data-testid="fleet-enrollment-pending"
    >
      <div>
        <p className="eyebrow">Enrollment state unavailable</p>
        <strong>Waiting for the signed enrollment state</strong>
        <p className="muted">New enrollment is disabled until the database can be read.</p>
      </div>
    </section>
  ) : enrollmentRequests.length > 0 ? (
    <div data-testid="fleet-enrollment-pending">
      {enrollmentRequests.map((request) => (
        <section
          aria-live="polite"
          className="fleet-enrollment-pending"
          key={request.requestId}
        >
          <div>
            <p className="eyebrow">Enrollment requested</p>
            <strong>
              {request.state === "approved"
                ? "Approval received · finishing secure route"
                : `Waiting for ${request.serverLabel ?? request.ownerAgent} approval`}
            </strong>
            <p className="muted">
              Request <span className="mono">{request.requestId}</span> · expires{" "}
              {request.expiresAt}
            </p>
          </div>
        </section>
      ))}
    </div>
  ) : null;
  const enrollmentBlocked = enrollmentRequests === null || enrollmentRequests.length > 0;

  if (!hasDeployments) {
    return (
      <section className="fleet-empty" data-testid="fleet-empty">
        <div className="fleet-empty-card panel">
          {brand}
          <div className="fleet-empty-copy">
            <h2>{localRuntimeSetup ? "Set up Gents" : "Connect your agent"}</h2>
            <p className="muted">
              {localRuntimeSetup
                ? "Optionally create an agent on this machine, or skip local setup and connect a remote agent."
                : "Enroll with a remote agent using its authenticated server offer."}
            </p>
          </div>
          {localRuntimeSetup}
          {enrollmentNotice ?? (
            <details
              className="fleet-remote-disclosure"
              data-testid="fleet-remote-disclosure"
            >
              <summary aria-label="Connect a remote agent">
                {localRuntimeSetup
                  ? "Skip local setup and connect a remote agent…"
                  : "Connect agent"}
              </summary>
              <AddPeerForm
                addingPeer={addingPeer}
                disabled={starting}
                localError={null}
                onRequestStatusEnrollment={requestStatusEnrollment}
              />
            </details>
          )}
        </div>
      </section>
    );
  }

  return (
    <section className="fleet-dashboard" data-testid="fleet-dashboard">
      <header className="fleet-header">
        {brand}
        <div className="fleet-header-actions">
          {headerLeadingActions}
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
            disabled={enrollmentBlocked}
            onClick={() => {
              setShowAddPeer((value) => !value);
            }}
            type="button"
          >
            {enrollmentRequests?.length ? "Enrollment requested" : "Add Agent"}
          </button>
        </div>
      </header>

      {enrollmentNotice}

      {showAddPeer && !enrollmentBlocked ? (
        <section className="panel fleet-add-panel">
          {localRuntimeSetup}
          <AddPeerForm
            addingPeer={addingPeer}
            disabled={starting}
            localError={null}
            onRequestStatusEnrollment={requestStatusEnrollment}
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
                syncHealth={syncHealth}
                key={deployment.peerId}
                onOpenChat={onOpenChat}
                onOpenConfig={onOpenConfig}
                onRemovePeer={onRemovePeer}
                onRenamePeer={onRenamePeer}
                onSetupInference={
                  renderInferenceSetup ? setWizardDeployment : undefined
                }
              />
            ))}
          </tbody>
        </table>
      </div>

      <NetworkPanel api={api} />

      {activeWizardDeployment && renderInferenceSetup
        ? renderInferenceSetup(activeWizardDeployment, () =>
            setWizardDeployment(null),
          )
        : null}
    </section>
  );
}
