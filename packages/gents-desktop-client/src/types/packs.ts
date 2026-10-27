import type { PluginJobSettings } from "../generated/PluginJobSettings.js";

export type { InstalledPackView as InstalledPack } from "../generated/InstalledPackView.js";
export type { FoundPackView as FoundPack } from "../generated/FoundPackView.js";
export type { PackPluginSlotView as PackPluginSlot } from "../generated/PackPluginSlotView.js";
export type { PackInferenceProfileOption as PackSlotProfile } from "../generated/PackInferenceProfileOption.js";
export type { EditedDocuments as PackEditedChoice } from "../generated/EditedDocuments.js";
export type { PackInstallRequest } from "../generated/PackInstallRequest.js";
export type { PackWireRequest } from "../generated/PackWireRequest.js";
export type { DynamicWorkflowsView as DynamicWorkflows } from "../generated/DynamicWorkflowsView.js";
export type { DynamicWorkflowSummary } from "../generated/DynamicWorkflowSummary.js";
export type { InstalledWorkflowSummary } from "../generated/InstalledWorkflowSummary.js";
export type { DynamicWorkflowView as DynamicWorkflow } from "../generated/DynamicWorkflowView.js";
export type { WorkflowPromoteRequest } from "../generated/WorkflowPromoteRequest.js";
export type { WorkflowPromoteView as WorkflowPromoted } from "../generated/WorkflowPromoteView.js";

/* The bridge's wire commands return the JSON `gents pack wire` reports, which
   the bridge does not type (`serde_json::Value`); these are its shapes. */

/** What wiring changed, answer first; `job` is the block the person consents to. */
export type WireReport = {
  changed: string[];
  job: PluginJobSettings | null;
  warnings: string[];
  next_call: string | null;
};

export type PackWireResult = {
  plugin: string;
  agent: string;
  preview: boolean;
  authority: string | null;
  report: WireReport;
};

export type PackWireSources = {
  plugins: { plugin: string; pack: string | null; job: boolean }[];
  target_packs: { pack: string; targets: string[] }[];
};
