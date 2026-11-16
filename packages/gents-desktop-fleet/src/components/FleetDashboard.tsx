import { useEffect, useRef, useState, type ReactNode } from "react";

import type {
  BootstrapSummary,
  DeploymentView,
  ClientAccessRequestView,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import type { FleetCopy } from "../copy.js";
import { needsInferenceSetup } from "../fleetMetrics.js";
import { AddPeerForm, type AddPeerFormProps } from "./AddPeerForm.js";
import { FleetRow } from "./FleetRow.js";

export type FleetDashboardProps = {
  addingPeer: boolean;
  bootstrap: BootstrapSummary | null;
  deployments: DeploymentView[];
  accessRequests: ClientAccessRequestView[] | null;
  loading: boolean;
  syncHealth?: SyncHealthView | null;
  starting: boolean;
  onRequestRuntimeAccess: AddPeerFormProps["onRequestRuntimeAccess"];
  onOpenChat: (nodeDid: string) => void;
  onOpenConfig: (nodeDid: string) => void;
  onRemoveRuntime?: (nodeDid: string) => Promise<unknown> | void;
  onRenameRuntime?: (nodeDid: string, label: string) => Promise<unknown> | void;
  brand?: ReactNode;
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
  accessRequests,
  syncHealth = null,
  starting,
  onRequestRuntimeAccess,
  onOpenChat,
  onOpenConfig,
  onRemoveRuntime,
  onRenameRuntime,
  brand,
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
    ? (deployments.find(
        (entry) => entry.nodeDid === wizardDeployment.nodeDid,
      ) ?? wizardDeployment)
    : null;
  useEffect(() => {
    if (
      !renderInferenceSetup ||
      !deploymentNeedingInference ||
      !deploymentNeedingInference.local ||
      autoPromptedInference.current.has(deploymentNeedingInference.nodeDid)
    ) {
      return;
    }
    autoPromptedInference.current.add(deploymentNeedingInference.nodeDid);
    setWizardDeployment(deploymentNeedingInference);
  }, [deploymentNeedingInference, renderInferenceSetup]);

  async function requestRuntimeAccess(serverAddress: string) {
    const request = await onRequestRuntimeAccess(serverAddress);
    setShowAddPeer(false);
    return request;
  }

  const enrollmentNotice = accessRequests
    ?.filter((r) => r.state !== "approved")
    .map((request) => (
      <section
        key={request.requestId}
        aria-live="polite"
        className="fleet-enrollment-pending"
      >
        <strong>
          {request.state === "pending"
            ? "Waiting for administrator approval"
            : `Access ${request.state}`}
        </strong>
        <p className="muted">{request.nodeDid}</p>
        <p className="muted">
          {request.state === "pending"
            ? "Ask the node administrator to approve this device."
            : "Request access again from the node’s address."}
        </p>
      </section>
    ));
  if (!hasDeployments) {
    return (
      <section className="fleet-empty" data-testid="fleet-empty">
        <div className="fleet-empty-card panel">
          {brand}
          <div className="fleet-empty-copy">
            <h2>{localRuntimeSetup ? "Set up Gents" : "Connect your node"}</h2>
            <p className="muted">
              {localRuntimeSetup
                ? "Optionally create a node on this machine, or skip local setup and connect a remote node."
                : "Enter a runtime server address and request administrator approval."}
            </p>
          </div>
          {localRuntimeSetup}
          {enrollmentNotice}
          {accessRequests !== null ? (
            <details
              className="fleet-remote-disclosure"
              data-testid="fleet-remote-disclosure"
            >
              <summary aria-label="Connect a remote node">
                {localRuntimeSetup
                  ? "Skip local setup and connect a remote node…"
                  : "Connect node"}
              </summary>
              <AddPeerForm
                addingPeer={addingPeer}
                disabled={starting}
                localError={null}
                onRequestRuntimeAccess={requestRuntimeAccess}
              />
            </details>
          ) : null}
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
          <button
            className="primary-button"
            disabled={false}
            onClick={() => {
              setShowAddPeer((value) => !value);
            }}
            type="button"
          >
            Add Node
          </button>
        </div>
      </header>

      {enrollmentNotice}

      {showAddPeer ? (
        <section className="panel fleet-add-panel">
          {localRuntimeSetup}
          <AddPeerForm
            addingPeer={addingPeer}
            disabled={starting}
            localError={null}
            onRequestRuntimeAccess={requestRuntimeAccess}
          />
        </section>
      ) : null}

      <div className="fleet-table-wrap">
        <table className="fleet-table">
          <thead>
            <tr>
              <th>Node</th>
              <th>Agents</th>
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
                key={deployment.nodeDid}
                onOpenChat={onOpenChat}
                onOpenConfig={onOpenConfig}
                onRemoveRuntime={onRemoveRuntime}
                onRenameRuntime={onRenameRuntime}
                onSetupInference={
                  renderInferenceSetup ? setWizardDeployment : undefined
                }
              />
            ))}
          </tbody>
        </table>
      </div>

      {activeWizardDeployment && renderInferenceSetup
        ? renderInferenceSetup(activeWizardDeployment, () =>
            setWizardDeployment(null),
          )
        : null}
    </section>
  );
}
