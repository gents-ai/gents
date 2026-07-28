import type {
  DesktopInterruptRequestRequest,
  DesktopListSubagentTreeRequest,
  DesktopPreviewInterruptCascadeRequest,
} from "../types.js";
import type {
  DesktopOperationsSnapshotRequest,
  DesktopResolveHoldRequest,
} from "../types/operations.js";
import { getDesktopApiAdapter } from "./adapter.js";
import type { DesktopApiAdapter } from "./types.js";

export function listWorkspace(
  subpath?: string | null,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).listWorkspace(subpath);
}

export function fetchRequestTimeline(
  agentDid: string,
  requestId: string,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).fetchRequestTimeline(agentDid, requestId);
}

export function listSubagentTree(
  request: DesktopListSubagentTreeRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).listSubagentTree(request);
}

export function listBackendsWithHealth(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).listBackendsWithHealth();
}

export function listMcpServicesWithHealth(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).listMcpServicesWithHealth();
}

export function probeMcpService(serviceId: string, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).probeMcpService(serviceId);
}

export function fetchOperationsSnapshot(
  request: DesktopOperationsSnapshotRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).fetchOperationsSnapshot(request);
}

export function previewInterruptCascade(
  request: DesktopPreviewInterruptCascadeRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).previewInterruptCascade(request);
}

export function interruptRequest(
  request: DesktopInterruptRequestRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).interruptRequest(request);
}

export function listToolCallHolds(agentDid: string, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).listToolCallHolds(agentDid);
}

export function resolveToolCallHold(
  request: DesktopResolveHoldRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).resolveToolCallHold(request);
}
