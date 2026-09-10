import type { DesktopInterruptRequest as GeneratedDesktopInterruptRequest } from "../generated/DesktopInterruptRequest.js";
import type { DesktopListSubagentTreeRequest as GeneratedDesktopListSubagentTreeRequest } from "../generated/DesktopListSubagentTreeRequest.js";
import type { DesktopOperationsSnapshotRequest as GeneratedDesktopOperationsSnapshotRequest } from "../generated/DesktopOperationsSnapshotRequest.js";
import type { DesktopPreviewInterruptCascadeRequest as GeneratedDesktopPreviewInterruptCascadeRequest } from "../generated/DesktopPreviewInterruptCascadeRequest.js";
import type { DesktopProbeMcpServiceRequest as GeneratedDesktopProbeMcpServiceRequest } from "../generated/DesktopProbeMcpServiceRequest.js";

type RequestInput<T> = {
  [K in keyof T as null extends T[K] ? never : K]: T[K];
} & {
  [K in keyof T as null extends T[K] ? K : never]?: T[K];
};

export type { ActiveRequestView } from "../generated/ActiveRequestView.js";
export type { ActiveToolCallView } from "../generated/ActiveToolCallView.js";
export type { BackgroundedToolView } from "../generated/BackgroundedToolView.js";
export type { CascadeAffectedRequest } from "../generated/CascadeAffectedRequest.js";
export type { CascadeCancelPreview } from "../generated/CascadeCancelPreview.js";
export type { DerivedCancelCauseView } from "../generated/DerivedCancelCauseView.js";
export type { DesktopOperationsSnapshot } from "../generated/DesktopOperationsSnapshot.js";
export type { InterruptRequestResult } from "../generated/InterruptRequestResult.js";
export type { MCPServiceHealthView } from "../generated/MCPServiceHealthView.js";
export type { McpServiceProbeResult } from "../generated/McpServiceProbeResult.js";
export type { NativeExecutorStatusView } from "../generated/NativeExecutorStatusView.js";
export type { RuntimeLivenessView } from "../generated/RuntimeLivenessView.js";
export type { StuckWorkDiagnosticView } from "../generated/StuckWorkDiagnosticView.js";
export type { SubagentEdgeView } from "../generated/SubagentEdgeView.js";
export type { SubagentNodeView } from "../generated/SubagentNodeView.js";
export type { SubagentTreeView } from "../generated/SubagentTreeView.js";
export type { WorkspaceEntryView } from "../generated/WorkspaceEntryView.js";
export type { WorkspaceListingView } from "../generated/WorkspaceListingView.js";

export type DesktopInterruptRequestRequest =
  RequestInput<GeneratedDesktopInterruptRequest>;
export type DesktopListSubagentTreeRequest =
  RequestInput<GeneratedDesktopListSubagentTreeRequest>;
export type DesktopOperationsSnapshotRequest =
  RequestInput<GeneratedDesktopOperationsSnapshotRequest>;
export type DesktopPreviewInterruptCascadeRequest =
  RequestInput<GeneratedDesktopPreviewInterruptCascadeRequest>;
export type DesktopProbeMcpServiceRequest =
  RequestInput<GeneratedDesktopProbeMcpServiceRequest>;
