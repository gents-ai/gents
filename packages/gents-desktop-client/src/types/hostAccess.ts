export type { BindAccess as AllowedFolderAccess } from "../generated/BindAccess.js";
export type { AllowedDir as AllowedFolder } from "../generated/AllowedDir.js";
export type { AllowedFoldersView as AllowedFolders } from "../generated/AllowedFoldersView.js";
export type { PluginApprovalView as PluginApprovalRequest } from "../generated/PluginApprovalView.js";

export type PluginApprovalDecision = "once" | "file" | "always" | "deny";
