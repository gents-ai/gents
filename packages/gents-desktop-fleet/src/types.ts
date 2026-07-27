/** Loose deployment shapes — host may pass fuller DesktopRuntimeSnapshot views. */
export type ToolSelectionView = {
  selectionId?: string | null;
  enableFileTools?: boolean | null;
  fileToolsMode?: string | null | undefined;
  enableBash?: boolean | null;
  bashMode?: string | null | undefined;
  enableMetaTools?: boolean | null;
  cliToolNames?: Array<string | null | undefined> | null;
  allowedMcpServiceIds?: Array<string | null | undefined> | null;
  [key: string]: unknown;
};

export type DeploymentView = {
  peerId?: string;
  label?: string;
  agentDid?: string;
  dialSucceeded?: boolean;
  lastError?: string | null;
  runtime?: {
    processState?: string | null;
    reconcilePhase?: string | null;
    lastReconcileError?: string | null;
    [key: string]: unknown;
  } | null;
  toolSelections?: ToolSelectionView[];
  inferenceBackends?: Array<{
    name?: string | null;
    backendId?: string | null;
    enabled?: boolean | null;
    [key: string]: unknown;
  }>;
  [key: string]: unknown;
};
