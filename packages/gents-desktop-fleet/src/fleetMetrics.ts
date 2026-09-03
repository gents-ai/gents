import type {
  DeploymentView,
  SyncHealthView,
  ToolSelectionView,
} from "@source-inc/gents-desktop-client";
import {
  isLocalRuntimeSource,
  projectDeploymentOperationalState,
} from "@source-inc/gents-desktop-client";

export { isLocalRuntimeSource } from "@source-inc/gents-desktop-client";

export type StatusTone = "green" | "yellow" | "red";

export type ToolIconKind = "file" | "bash" | "meta" | "cli";

export type ToolIcon = {
  kind: ToolIconKind;
  tone: "readonly" | "readwrite" | "meta";
  title: string;
};

export function deploymentStatus(
  deployment: DeploymentView,
  syncHealth: SyncHealthView | null = null,
): {
  title: string;
  tone: StatusTone;
  label: string;
  lastError: string | null;
  chatReady: boolean;
} {
  const operational = projectDeploymentOperationalState(
    deployment,
    null,
    syncHealth,
  );
  const lastError =
    deployment.lastError ?? deployment.runtime?.lastReconcileError ?? null;
  const title = [
    operational.transport.detail,
    operational.route.detail,
    operational.sync.detail,
    operational.behavior.detail,
    operational.reconcile.detail,
  ]
    .filter(Boolean)
    .join(" | ");
  return {
    title,
    tone:
      operational.summary.kind === "ready"
        ? "green"
        : operational.summary.kind === "blocked"
          ? "red"
          : "yellow",
    label: operational.summary.shortLabel,
    lastError,
    chatReady: operational.admissionBlocker === null,
  };
}

export function inferenceBackendTitle(deployment: DeploymentView) {
  if (deployment.source === "enrollment") {
    return projectDeploymentOperationalState(deployment).behaviorReadiness
      .kind !== "unknown"
      ? "Backend details stay on the agent host; this runtime reports authoritative behavior readiness"
      : "Backend details stay on the agent host; runtime readiness is currently unavailable";
  }
  const labels = (deployment.inferenceBackends ?? [])
    .filter((backend) => backend.enabled !== false)
    .map((backend) => backend.name ?? backend.backendId);
  return labels.length
    ? `Configured inference backends: ${labels.join(", ")}`
    : "No configured inference backends";
}

export function needsInferenceSetup(deployment: DeploymentView): boolean {
  // Enrolled clients do not receive non-branchable backend documents and
  // cannot configure them locally. Runtime-authored readiness is authoritative.
  if (deployment.source === "enrollment") return false;
  return !deployment.inferenceBackends.some(
    (backend) => backend.enabled !== false && backend.models.length > 0,
  );
}

export function toolCeilingIcons(
  selections: ToolSelectionView[],
  selectedToolSelectionId?: string | null,
  serverCeiling?: string | null,
): ToolIcon[] {
  const source = selectedToolSelectionId
    ? selections.filter(
        (selection) => selection.selectionId === selectedToolSelectionId,
      )
    : [];
  const icons: ToolIcon[] = [];
  const ceilingSuffix = serverCeiling
    ? ` Server ceiling: ${displayToolCeiling(serverCeiling)}.`
    : "";
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
  const allowedMetaServices = uniqueValues(
    source
      .filter((selection) => selection.enableMetaTools)
      .flatMap((selection) => selection.allowedMcpServiceIds ?? []),
  );
  const unrestrictedMetaServices = source.some(
    (selection) =>
      selection.enableMetaTools &&
      (selection.allowedMcpServiceIds ?? []).length === 0,
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
      } workspace files.${ceilingSuffix}`,
    });
  }
  if (bestBashMode) {
    icons.push({
      kind: "bash",
      tone: bestBashMode === "readwrite" ? "readwrite" : "readonly",
      title: `Shell (${modeTitle(bestBashMode)}): run terminal commands for diagnostics${
        bestBashMode === "readwrite" ? " and changes" : ""
      }.${ceilingSuffix}`,
    });
  }
  if (allowedMetaServices.length || unrestrictedMetaServices) {
    icons.push({
      kind: "meta",
      tone: "meta",
      title: unrestrictedMetaServices
        ? "MCP services: discover and call all online MCP services."
        : `MCP services: discover and call ${allowedMetaServices.join(", ")}.`,
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

function uniqueValues(values: Array<string | null | undefined>) {
  return Array.from(
    new Set(
      values
        .filter((value): value is string => Boolean(value && value.trim()))
        .map((value) => value.trim()),
    ),
  ).sort();
}

export function formatRelativeTime(value?: string | null) {
  if (!value) {
    return "unknown";
  }
  const timestamp = new Date(value).getTime();
  if (!Number.isFinite(timestamp)) {
    return "unknown";
  }
  const elapsedSeconds = Math.max(
    0,
    Math.floor((Date.now() - timestamp) / 1000),
  );
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
