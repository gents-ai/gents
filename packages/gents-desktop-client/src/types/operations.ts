import type { DesktopInterruptRequest as GeneratedDesktopInterruptRequest } from "../generated/DesktopInterruptRequest.js";
import type { DesktopOperationsSnapshotRequest as GeneratedDesktopOperationsSnapshotRequest } from "../generated/DesktopOperationsSnapshotRequest.js";
import type { DesktopProbeMcpServiceRequest as GeneratedDesktopProbeMcpServiceRequest } from "../generated/DesktopProbeMcpServiceRequest.js";
import type { DesktopSessionProvenanceRequest as GeneratedDesktopSessionProvenanceRequest } from "../generated/DesktopSessionProvenanceRequest.js";

type RequestInput<T> = {
  [K in keyof T as null extends T[K] ? never : K]: T[K];
} & {
  [K in keyof T as null extends T[K] ? K : never]?: T[K];
};

export type { ActiveRequestView } from "../generated/ActiveRequestView.js";
export type { ActiveToolCallView } from "../generated/ActiveToolCallView.js";
export type { BackgroundedToolView } from "../generated/BackgroundedToolView.js";
export type { CausedRequestView } from "../generated/CausedRequestView.js";
export type { DerivedCancelCauseView } from "../generated/DerivedCancelCauseView.js";
export type { DesktopOperationsSnapshot } from "../generated/DesktopOperationsSnapshot.js";
export type { InterruptRequestResult } from "../generated/InterruptRequestResult.js";
export type { MCPServiceHealthView } from "../generated/MCPServiceHealthView.js";
export type { McpServiceProbeResult } from "../generated/McpServiceProbeResult.js";
export type { NativeExecutorStatusView } from "../generated/NativeExecutorStatusView.js";
export type { RuntimeLivenessView } from "../generated/RuntimeLivenessView.js";
export type { SessionProvenanceView } from "../generated/SessionProvenanceView.js";
export type { StuckWorkDiagnosticView } from "../generated/StuckWorkDiagnosticView.js";
export type { WorkspaceEntryView } from "../generated/WorkspaceEntryView.js";
export type { WorkspaceListingView } from "../generated/WorkspaceListingView.js";

export type DesktopInterruptRequestRequest =
  RequestInput<GeneratedDesktopInterruptRequest>;
export type DesktopOperationsSnapshotRequest =
  RequestInput<GeneratedDesktopOperationsSnapshotRequest>;
export type DesktopProbeMcpServiceRequest =
  RequestInput<GeneratedDesktopProbeMcpServiceRequest>;
export type DesktopSessionProvenanceRequest =
  RequestInput<GeneratedDesktopSessionProvenanceRequest>;
