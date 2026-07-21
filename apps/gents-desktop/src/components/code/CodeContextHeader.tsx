import type { DeploymentView, ToolSelectionView } from "../../lib/types";

// The multi-agent crux: an agent codes in ITS OWN tool root on ITS host, within
// its permission/sandbox boundary. This header makes that boundary explicit
// before the operator directs a coding task — resolved entirely from data
// already on DeploymentView (no bridge work).
//
// Honesty notes: the boundary shown is the selection governing the behavior the
// next send will use (the selected behavior, falling back to the deployment
// default). If that behavior resolves no tool selection, file/bash tools are
// off — we say so rather than showing some other selection's boundary. What's
// shown is the PERSISTED selection; the host's operator ceiling may clamp it
// further at reconcile time.
function governingToolSelection(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): ToolSelectionView | null {
  if (!deployment) {
    return null;
  }
  const behaviorId =
    selectedBehaviorId ??
    deployment.defaultBehaviorId ??
    deployment.agentPrincipal.defaultBehaviorId;
  const behavior =
    deployment.behaviors.find((candidate) => candidate.behaviorId === behaviorId) ??
    deployment.behaviors.find(
      (candidate) =>
        candidate.behaviorId ===
        (deployment.defaultBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId),
    );
  if (!behavior?.toolSelectionId) {
    return null;
  }
  return (
    deployment.toolSelections.find(
      (selection) => selection.selectionId === behavior.toolSelectionId,
    ) ?? null
  );
}

function fileModeLabel(selection: ToolSelectionView | null): string {
  if (!selection?.enableFileTools) {
    return "off";
  }
  switch (selection.fileToolsMode) {
    case "ReadWrite":
      return "read / write";
    case "ReadOnly":
    case undefined:
    case null:
      return "read-only";
    case "Off":
      return "off";
    default:
      return selection.fileToolsMode;
  }
}

function bashModeLabel(selection: ToolSelectionView | null): string {
  if (!selection?.enableBash) {
    return "off";
  }
  switch (selection.bashMode) {
    case "Unrestricted":
    case "ReadWrite":
      return "unrestricted";
    case "ReadOnly":
    case undefined:
    case null:
      return "read-only";
    case "Off":
      return "off";
    default:
      return selection.bashMode;
  }
}

function shortPeerId(peerId: string): string {
  return peerId.length > 12 ? `${peerId.slice(0, 12)}…` : peerId;
}

export function CodeContextHeader({
  deployment,
  selectedBehaviorId,
  onBackToChat,
}: {
  deployment: DeploymentView | null;
  selectedBehaviorId?: string | null;
  onBackToChat: () => void;
}) {
  const selection = governingToolSelection(deployment, selectedBehaviorId ?? null);
  // The P2P identity is the honest "where it runs" answer: saved GraphQL
  // endpoints can be agent-relative (loopback on the AGENT's machine), so an
  // endpoint host would misleadingly read as the operator's machine.
  const host = deployment
    ? `${deployment.label} · ${shortPeerId(deployment.peerId)}`
    : null;
  const workingDir = selection?.fileToolRoot?.trim() || null;

  return (
    <header className="code-context" data-testid="code-context-header">
      <div className="code-context-main">
        <div className="code-context-title">
          <span className="eyebrow">Code</span>
          <h2>
            {deployment?.agentPrincipal.displayName ??
              deployment?.label ??
              "No agent selected"}
          </h2>
        </div>
        <button
          className="ghost-button"
          data-testid="code-back-to-chat"
          onClick={onBackToChat}
          type="button"
        >
          Back to Chat
        </button>
      </div>
      <dl className="code-context-facts">
        <div>
          <dt>Agent host</dt>
          <dd className="mono" data-testid="code-context-host">
            {host ?? "—"}
          </dd>
        </div>
        <div>
          <dt>Working directory</dt>
          <dd className="mono" data-testid="code-context-workdir">
            {selection
              ? (workingDir ?? "host operator root")
              : "none (files & bash off)"}
          </dd>
        </div>
        <div>
          <dt>Files</dt>
          <dd data-testid="code-context-files">{fileModeLabel(selection)}</dd>
        </div>
        <div>
          <dt>Bash</dt>
          <dd data-testid="code-context-bash">{bashModeLabel(selection)}</dd>
        </div>
        <div>
          <dt>Command network</dt>
          <dd>{selection ? (selection.commandNetworkMode ?? "inherit") : "—"}</dd>
        </div>
      </dl>
      <p className="code-context-note muted small">
        The agent edits files and runs commands on its own host, within the boundary
        persisted above — the host operator&apos;s tool ceiling may restrict it further.
        Direct it below; edits and command output stream back.
      </p>
    </header>
  );
}
