import type { DeploymentView, ToolSelectionView } from "../../lib/types";

export type StatusTone = "green" | "yellow" | "red";

export type ToolIconKind = "file" | "bash" | "meta" | "cli";

export type ToolIcon = {
  kind: ToolIconKind;
  tone: "readonly" | "readwrite" | "meta";
  title: string;
};

export function deploymentStatus(deployment: DeploymentView): {
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

export function inferenceBackendTitle(deployment: DeploymentView) {
  const labels = deployment.inferenceBackends
    .filter((backend) => backend.enabled !== false)
    .map((backend) => backend.name ?? backend.backendId);
  return labels.length
    ? `Configured inference backends: ${labels.join(", ")}`
    : "No configured inference backends";
}

export function toolCeilingIcons(
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
  // Only claim a server ceiling when the caller could actually know it —
  // init.json is a fact about the local machine, not remote peers.
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
      selection.enableMetaTools && (selection.allowedMcpServiceIds ?? []).length === 0,
  );
  const cliTools = uniqueValues(source.flatMap((selection) => selection.cliToolNames));

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
      ["readwrite", "read-write", "unrestricted"].includes((value ?? "").toLowerCase()),
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

/**
 * The init.json tool ceiling describes THIS machine's runtime — only
 * deployments sourced from the local runtime may claim it in their tooltip.
 */
export function isLocalRuntimeSource(source?: string | null): boolean {
  return source === "local-standard" || source === "server-status";
}

export function formatRelativeTime(value?: string | null) {
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
