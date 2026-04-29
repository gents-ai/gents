import { useState, type FormEvent } from "react";

import sourceMarkUrl from "../../assets/source-mark-light.png";
import { isTerminalTurnState } from "../../lib/chat-shell";
import type {
  BootstrapSummary,
  DeploymentView,
  P2PHealth,
  PeerAddRequest,
  ToolSelectionView,
} from "../../lib/types";
import { displayAgentIdentity, displayBehaviorLabel } from "../../lib/types";
import { parsePeerConnectionJson, validateAgentDid } from "./peerConnectionImport";

type FleetDashboardProps = {
  addingPeer: boolean;
  bootstrap: BootstrapSummary | null;
  deployments: DeploymentView[];
  loading: boolean;
  p2pHealth: P2PHealth | null;
  repairingP2P: boolean;
  starting: boolean;
  onAddPeer: (request: PeerAddRequest) => Promise<unknown>;
  onOpenChat: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
  onRepairP2P: () => Promise<unknown>;
};

type StatusTone = "green" | "yellow" | "red";

type ToolIconKind = "file" | "bash" | "meta" | "cli";

type ToolIcon = {
  kind: ToolIconKind;
  tone: "readonly" | "readwrite" | "meta";
  title: string;
};

const DEFAULT_PEER_FORM = {
  label: "",
  agentDid: "",
  addr: "",
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
  onOpenChat,
  onOpenConfig,
  onRepairP2P,
}: FleetDashboardProps) {
  const [showAddPeer, setShowAddPeer] = useState(false);
  const [peerForm, setPeerForm] = useState(DEFAULT_PEER_FORM);
  const [localError, setLocalError] = useState<string | null>(null);
  const hasDeployments = deployments.length > 0;

  async function submitPeer(event: FormEvent) {
    event.preventDefault();
    setLocalError(null);
    try {
      await onAddPeer({
        ...peerForm,
        agentDid: validateAgentDid(peerForm.agentDid),
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

function BrandLockup() {
  return (
    <div className="fleet-brand">
      <img alt="Source" className="fleet-brand-logo" src={sourceMarkUrl} />
      <div>
        <p className="eyebrow">Source Network</p>
        <h1>Defra Agent</h1>
        <p className="muted">Fleet Dashboard</p>
      </div>
    </div>
  );
}

type AddPeerFormProps = {
  addingPeer: boolean;
  disabled: boolean;
  localError: string | null;
  peerForm: PeerAddRequest;
  onPeerFormChange: (value: PeerAddRequest) => void;
  onSubmit: (event: FormEvent) => void;
};

function AddPeerForm({
  addingPeer,
  disabled,
  localError,
  peerForm,
  onPeerFormChange,
  onSubmit,
}: AddPeerFormProps) {
  const [connectionJson, setConnectionJson] = useState("");
  const [importStatus, setImportStatus] = useState<string | null>(null);

  function updateConnectionJson(value: string) {
    setConnectionJson(value);
    if (!value.trim()) {
      setImportStatus(null);
      return;
    }

    try {
      onPeerFormChange(parsePeerConnectionJson(value));
      setImportStatus("Imported connection JSON");
    } catch (error) {
      setImportStatus(String(error));
    }
  }

  return (
    <form className="fleet-add-form" onSubmit={onSubmit}>
      <label className="field fleet-import-field">
        <span>Connection JSON</span>
        <textarea
          className="mono"
          data-testid="fleet-add-connection-json"
          disabled={disabled || addingPeer}
          onChange={(event) => updateConnectionJson(event.currentTarget.value)}
          placeholder='{"label":"api-gateway","agentDid":"did:key:z6Mk...","addr":"/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."}'
          value={connectionJson}
        />
        {importStatus ? (
          <span className="fleet-import-status muted">{importStatus}</span>
        ) : null}
      </label>
      <label className="field">
        <span>Agent label</span>
        <input
          data-testid="fleet-add-label"
          disabled={disabled || addingPeer}
          onChange={(event) =>
            onPeerFormChange({ ...peerForm, label: event.currentTarget.value })
          }
          placeholder="api-gateway"
          value={peerForm.label}
        />
      </label>
      <label className="field">
        <span>Agent DID</span>
        <input
          className="mono"
          data-testid="fleet-add-agent-did"
          disabled={disabled || addingPeer}
          onChange={(event) =>
            onPeerFormChange({
              ...peerForm,
              agentDid: event.currentTarget.value,
            })
          }
          placeholder="did:key:z6Mk..."
          value={peerForm.agentDid}
        />
      </label>
      <label className="field">
        <span>P2P address</span>
        <input
          className="mono"
          data-testid="fleet-add-addr"
          disabled={disabled || addingPeer}
          onChange={(event) =>
            onPeerFormChange({ ...peerForm, addr: event.currentTarget.value })
          }
          placeholder="/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."
          value={peerForm.addr}
        />
      </label>
      {localError ? <p className="fleet-inline-error">{localError}</p> : null}
      <div className="fleet-add-actions">
        <button
          className="primary-button"
          data-testid="fleet-add-submit"
          disabled={
            disabled ||
            addingPeer ||
            !peerForm.label.trim() ||
            !peerForm.agentDid.trim() ||
            !peerForm.addr.trim()
          }
          type="submit"
        >
          {addingPeer
            ? "Adding..."
            : disabled
              ? "Preparing..."
              : "Add Agent Connection"}
        </button>
      </div>
    </form>
  );
}

type FleetRowProps = {
  bootstrap: BootstrapSummary | null;
  deployment: DeploymentView;
  p2pHealth: P2PHealth | null;
  repairingP2P: boolean;
  onOpenChat: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
  onRepairP2P: () => Promise<unknown>;
};

function FleetRow({
  bootstrap,
  deployment,
  p2pHealth,
  repairingP2P,
  onOpenChat,
  onOpenConfig,
  onRepairP2P,
}: FleetRowProps) {
  const status = deploymentStatus(deployment);
  const enabledTaskCount = deployment.tasks.filter(
    (task) => task.enabled !== false,
  ).length;
  const backendCount = deployment.inferenceBackends.filter(
    (backend) => backend.enabled !== false,
  ).length;
  const openWorkCount = deployment.conversations.filter(
    (conversation) =>
      conversation.turnState && !isTerminalTurnState(conversation.turnState),
  ).length;
  const defaultBehavior = deployment.behaviors.find(
    (behavior) =>
      behavior.behaviorId ===
      (deployment.defaultBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId),
  );
  const toolIcons = toolCeilingIcons(
    deployment.toolSelections,
    defaultBehavior?.toolSelectionId,
    bootstrap?.initToolCeiling,
  );
  const agentIdentity = displayAgentIdentity(deployment.agentDid);
  const defaultBehaviorLabel = displayBehaviorLabel(
    deployment.defaultBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId,
  );
  const p2pLastUpdate = p2pHealth?.lastOkAt ?? p2pHealth?.lastFailureAt ?? null;
  const canRepairP2P = !deployment.dialSucceeded || Boolean(deployment.lastError);

  return (
    <tr data-testid={`fleet-row-${deployment.peerId}`}>
      <td>
        <div className="fleet-agent-cell">
          <span
            className={`fleet-status-dot ${status.tone}`}
            title={status.title}
          />
          <div className="fleet-agent-copy">
            <button
              className="fleet-agent-name"
              data-testid={`fleet-chat-name-${deployment.peerId}`}
              onClick={() => onOpenChat(deployment.agentDid)}
              title={`Open ${deployment.label} chat`}
              type="button"
            >
              {deployment.agentPrincipal.displayName ?? deployment.label}
            </button>
            <span className="muted mono">
              {[
                agentIdentity,
                defaultBehaviorLabel ? `default: ${defaultBehaviorLabel}` : null,
              ]
                .filter(Boolean)
                .join(" | ")}
            </span>
          </div>
        </div>
      </td>
      <td>
        <Metric value={deployment.behaviors.length} label="total" />
      </td>
      <td>
        <Metric value={enabledTaskCount} label="enabled" />
      </td>
      <td>
        <Metric
          label={backendCount === 1 ? "backend" : "backends"}
          title={inferenceBackendTitle(deployment)}
          value={backendCount}
        />
      </td>
      <td>
        <ToolIconStrip icons={toolIcons} />
      </td>
      <td>
        <Metric title="Processing conversations" value={openWorkCount} />
      </td>
      <td title="Last desktop P2P health probe">
        {formatRelativeTime(p2pLastUpdate)}
      </td>
      <td className="fleet-actions-cell">
        <div className="fleet-row-actions">
          <button
            aria-label={`Open ${deployment.label} chat`}
            className="primary-button fleet-table-action"
            data-testid={`fleet-chat-${deployment.peerId}`}
            onClick={() => onOpenChat(deployment.agentDid)}
            title="Open chat"
            type="button"
          >
            <ChatIcon />
          </button>
          <button
            aria-label={`Configure ${deployment.label}`}
            className="ghost-button fleet-table-action"
            data-testid={`fleet-config-${deployment.peerId}`}
            onClick={() => onOpenConfig(deployment.agentDid)}
            title="Configure agent"
            type="button"
          >
            <ConfigIcon />
          </button>
          <button
            aria-label={
              canRepairP2P
                ? `Repair ${deployment.label} P2P`
                : `${deployment.label} P2P healthy`
            }
            className="ghost-button fleet-table-action"
            data-testid={`fleet-repair-${deployment.peerId}`}
            disabled={!canRepairP2P || repairingP2P}
            onClick={() => void onRepairP2P()}
            title={canRepairP2P ? "Repair P2P" : "P2P healthy"}
            type="button"
          >
            <RepairIcon />
          </button>
        </div>
      </td>
    </tr>
  );
}

function Metric({
  label,
  title,
  value,
}: {
  label?: string;
  title?: string;
  value: number;
}) {
  return (
    <span className="fleet-metric" title={title}>
      {value}
      {label ? <span>{label}</span> : null}
    </span>
  );
}

function ToolIconStrip({ icons }: { icons: ToolIcon[] }) {
  if (!icons.length) {
    return <span className="muted">none</span>;
  }

  return (
    <div className="fleet-tool-icons">
      {icons.map((icon) => (
        <span
          className={`fleet-tool-icon ${icon.tone}`}
          key={`${icon.kind}-${icon.title}`}
          title={icon.title}
        >
          <ToolIconGlyph kind={icon.kind} />
        </span>
      ))}
    </div>
  );
}

function deploymentStatus(deployment: DeploymentView): {
  title: string;
  tone: StatusTone;
} {
  const p2p = deployment.dialSucceeded ? "connected" : "saved";
  const runtime = deployment.runtime?.processState ?? "unknown";
  const reconcile = deployment.runtime?.reconcilePhase ?? "unknown";
  const lastError =
    deployment.lastError ?? deployment.runtime?.lastReconcileError ?? null;
  const title = [
    `P2P ${p2p}`,
    `runtime ${runtime}`,
    `reconcile ${reconcile}`,
    lastError ? `error ${lastError}` : null,
  ]
    .filter(Boolean)
    .join(" | ");

  if (!deployment.dialSucceeded || lastError) {
    return { title, tone: "red" };
  }

  if (reconcile !== "idle" && reconcile !== "unknown") {
    return { title, tone: "yellow" };
  }

  return { title, tone: "green" };
}

function inferenceBackendTitle(deployment: DeploymentView) {
  const labels = deployment.inferenceBackends
    .filter((backend) => backend.enabled !== false)
    .map((backend) => backend.name ?? backend.backendId);
  return labels.length
    ? `Configured inference backends: ${labels.join(", ")}`
    : "No configured inference backends";
}

function toolCeilingIcons(
  selections: ToolSelectionView[],
  selectedToolSelectionId?: string | null,
  serverCeiling?: string | null,
): ToolIcon[] {
  const activeSelections =
    selectedToolSelectionId == null
      ? selections
      : selections.filter(
          (selection) => selection.selectionId === selectedToolSelectionId,
        );
  const source = activeSelections.length ? activeSelections : selections;
  const icons: ToolIcon[] = [];
  const ceilingLabel = displayToolCeiling(serverCeiling);
  const bestFileMode = strongestMode(
    source
      .filter((selection) => selection.enableFileTools)
      .map((selection) => selection.fileToolsMode),
  );
  const bestBashMode = strongestMode(
    source
      .filter((selection) => selection.enableBash)
      .map((selection) => selection.bashMode),
  );
  const metaDelegates = uniqueValues(
    source
      .filter((selection) => selection.enableMetaTools)
      .flatMap((selection) => selection.delegateTo),
  );
  const cliTools = uniqueValues(
    source.flatMap((selection) => selection.cliToolNames),
  );

  if (bestFileMode) {
    icons.push({
      kind: "file",
      tone: bestFileMode === "readwrite" ? "readwrite" : "readonly",
      title: `Files (${modeTitle(bestFileMode)}): inspect${
        bestFileMode === "readwrite" ? " and edit" : ""
      } workspace files. Server ceiling: ${ceilingLabel}.`,
    });
  }
  if (bestBashMode) {
    icons.push({
      kind: "bash",
      tone: bestBashMode === "readwrite" ? "readwrite" : "readonly",
      title: `Shell (${modeTitle(bestBashMode)}): run terminal commands for diagnostics${
        bestBashMode === "readwrite" ? " and changes" : ""
      }. Server ceiling: ${ceilingLabel}.`,
    });
  }
  if (metaDelegates.length) {
    icons.push({
      kind: "meta",
      tone: "meta",
      title: `Delegation: call configured MCP delegates (${metaDelegates.join(", ")}).`,
    });
  }
  if (cliTools.length) {
    icons.push({
      kind: "cli",
      tone: "readonly",
      title: `CLI tools: use configured command-line integrations (${cliTools.join(", ")}).`,
    });
  }

  return icons.slice(0, 4);
}

function strongestMode(values: Array<string | null | undefined>) {
  if (
    values.some((value) =>
      ["readwrite", "read-write", "unrestricted"].includes(
        (value ?? "").toLowerCase(),
      ),
    )
  ) {
    return "readwrite";
  }
  if (
    values.some((value) =>
      ["readonly", "read-only"].includes((value ?? "").toLowerCase()),
    )
  ) {
    return "readonly";
  }
  return null;
}

function modeTitle(mode: "readonly" | "readwrite") {
  return mode === "readwrite" ? "read/write" : "read-only";
}

function displayToolCeiling(value?: string | null) {
  switch ((value ?? "").toLowerCase()) {
    case "metaonly":
    case "meta-only":
      return "meta only";
    case "readwrite":
    case "read-write":
      return "read/write";
    case "readonly":
    case "read-only":
      return "read-only";
    default:
      return "unknown";
  }
}

function uniqueValues(values: string[]) {
  return Array.from(
    new Set(values.map((value) => value.trim()).filter(Boolean)),
  ).sort();
}

function formatRelativeTime(value?: string | null) {
  if (!value) {
    return "unknown";
  }
  const timestamp = new Date(value).getTime();
  if (!Number.isFinite(timestamp)) {
    return "unknown";
  }
  const elapsedSeconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  if (elapsedSeconds < 60) {
    return `${elapsedSeconds}s ago`;
  }
  const elapsedMinutes = Math.floor(elapsedSeconds / 60);
  if (elapsedMinutes < 60) {
    return `${elapsedMinutes}m ago`;
  }
  const elapsedHours = Math.floor(elapsedMinutes / 60);
  if (elapsedHours < 24) {
    return `${elapsedHours}h ago`;
  }
  return `${Math.floor(elapsedHours / 24)}d ago`;
}

function ChatIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z" />
    </svg>
  );
}

function ConfigIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 6h16" />
      <path d="M4 12h16" />
      <path d="M4 18h16" />
      <path d="M8 6v.01" />
      <path d="M14 12v.01" />
      <path d="M10 18v.01" />
    </svg>
  );
}

function RepairIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M20 7h-6v6" />
      <path d="M20 7l-7 7" />
      <path d="M4 17h6v-6" />
      <path d="M4 17l7-7" />
    </svg>
  );
}

function ToolIconGlyph({ kind }: { kind: ToolIconKind }) {
  if (kind === "file") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
        <path d="M14 2v6h6" />
      </svg>
    );
  }
  if (kind === "bash") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="m4 7 5 5-5 5" />
        <path d="M12 19h8" />
      </svg>
    );
  }
  if (kind === "meta") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="6" cy="12" r="2" />
        <circle cx="18" cy="6" r="2" />
        <circle cx="18" cy="18" r="2" />
        <path d="M8 12l8-6" />
        <path d="M8 12l8 6" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 5h16v14H4z" />
      <path d="M8 9h.01" />
      <path d="M11 9h.01" />
      <path d="M14 9h.01" />
      <path d="M8 14h8" />
    </svg>
  );
}
