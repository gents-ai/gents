import type { RequestAttachment } from "../generated/RequestAttachment.js";
import type { ProviderRecoveryRequest } from "../generated/ProviderRecoveryRequest.js";
import type { ProviderRecoveryPlan } from "../generated/ProviderRecoveryPlan.js";
import type { ProviderRecoveryApply } from "../generated/ProviderRecoveryApply.js";
import type { ProviderRecoveryResult } from "../generated/ProviderRecoveryResult.js";
import type { DesktopBackgroundToolRequest } from "../generated/DesktopBackgroundToolRequest.js";
import type { SessionForkBeforeRequest } from "../generated/SessionForkBeforeRequest.js";
import type { SessionForkResult } from "../generated/SessionForkResult.js";
import type { PendingMessageSteerRequest } from "../generated/PendingMessageSteerRequest.js";
import type { PendingQueueEditRequest } from "../generated/PendingQueueEditRequest.js";
import type { PendingQueueEditResult } from "../generated/PendingQueueEditResult.js";
import type { GoalControlRequest } from "../generated/GoalControlRequest.js";
import type { GoalControlResult } from "../generated/GoalControlResult.js";
import type { SessionGoalView } from "../generated/SessionGoalView.js";
import type { OauthProvider } from "../transport.js";
import type { DesktopJobControlRequest } from "../generated/DesktopJobControlRequest.js";
import type { JobControlOutcome } from "../generated/JobControlOutcome.js";
import type { ConfigComponentsApplyRequest } from "../generated/ConfigComponentsApplyRequest.js";
import type { ConfigComponentsPatchRequest } from "../generated/ConfigComponentsPatchRequest.js";
import type { EventSourceSaveRequest } from "../generated/EventSourceSaveRequest.js";
import type { EventSourceDeleteRequest } from "../generated/EventSourceDeleteRequest.js";
import type { BackendHealth } from "../types/backendHealth.js";
import type { ManagedServerStatus } from "../generated/ManagedServerStatus.js";
import type { ManagedServerResetResult } from "../generated/ManagedServerResetResult.js";
import type { HomeResetDisposition } from "../generated/HomeResetDisposition.js";
import type { ManagedServerToolCeiling } from "../generated/ManagedServerToolCeiling.js";
import type { ProviderAccountView } from "../generated/ProviderAccountView.js";
import type { AccountUsageView } from "../generated/AccountUsageView.js";
import type { InferenceSetupCatalog } from "../generated/InferenceSetupCatalog.js";
import type { InferenceDiscoveryRequest } from "../generated/InferenceDiscoveryRequest.js";
import type { InferenceDiscoveryResult } from "../generated/InferenceDiscoveryResult.js";
import type { InferenceRecommendationRequest } from "../generated/InferenceRecommendationRequest.js";
import type { InferenceBackendRecommendationRequest } from "../generated/InferenceBackendRecommendationRequest.js";
import type { InferenceModelRecommendation } from "../generated/InferenceModelRecommendation.js";
import type {
  NodeConfigSaveRequest,
  DefaultAgentSetRequest,
  BackendDeleteRequest,
  BackendSaveRequest,
  AgentDeleteRequest,
  ContextDeleteRequest,
  AgentSaveRequest,
  ChatSendRequest,
  ChatSendResult,
  CodexLoginResult,
  ClaudeLoginResult,
  GrokLoginResult,
  DesktopClientSnapshot,
  EnrollmentRequestView,
  DesktopInterruptRequestRequest,
  DesktopSessionProvenanceRequest,
  DesktopSessionSnapshot,
  SessionRenameRequest,
  SessionLiveDeltaView,
  SessionRowsView,
  SessionRowsWindow,
  TriggerDeleteRequest,
  TriggerSaveRequest,
  InferenceProbeResult,
  InferenceProfileDeleteRequest,
  InferenceProfileSaveRequest,
  InitSummary,
  InterruptRequestResult,
  MailboxItemView,
  MCPServiceHealthView,
  McpServiceProbeResult,
  NetworkStatusView,
  RequestResendResult,
  RequestTimelineView,
  ScheduleDeleteRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  SkillDeleteRequest,
  SkillSaveRequest,
  SessionProvenanceView,
  TaskDeleteRequest,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
  ToolsDeleteRequest,
  ToolsSaveRequest,
  ToolServiceDeleteRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
  ToolSurfaceExplanationView,
  WorkspaceListingView,
} from "../types.js";
import type {
  DesktopOperationsSnapshot,
  DesktopOperationsSnapshotRequest,
} from "../types/operations.js";
import type {
  FoundPack,
  InstalledPack,
  PackEditedChoice,
  PackInstallRequest,
  PackPluginSlot,
  PackSlotProfile,
  PackWireRequest,
  PackWireResult,
  PackWireSources,
} from "../types/packs.js";
import type {
  AllowedFolderAccess,
  AllowedFolders,
  PluginApprovalDecision,
  PluginApprovalRequest,
} from "../types/hostAccess.js";
import type {
  PackPluginCallRequest,
  UxDataCallRequest,
  UxDataCallResult,
  UxDataSurfacesRequest,
  UxPluginListing,
  UxPluginSource,
} from "../types/uxPlugins.js";

export type ManagedServerAuthorityInput = {
  toolCeiling: ManagedServerToolCeiling;
  toolRoot: string | null;
};

export type DesktopApiAdapter = {
  readAttachment(path: string): Promise<RequestAttachment>;
  requestAttachments(
    nodeDid: string,
    sessionId: string,
    requestId: string,
    messageKey?: string,
  ): Promise<RequestAttachment[]>;
  fetchSessionGoal: (
    nodeDid: string,
    sessionId: string,
  ) => Promise<SessionGoalView | null>;
  controlGoal: (request: GoalControlRequest) => Promise<GoalControlResult>;
  fetchDesktopSnapshot: () => Promise<DesktopClientSnapshot>;
  initLocalStandardRuntime: (request: {
    label: string;
    dangerouslyOverwrite: boolean;
    reset: boolean;
  }) => Promise<InitSummary>;
  startDesktopClient: () => Promise<DesktopClientSnapshot>;
  shutdownDesktopClient: () => Promise<DesktopClientSnapshot>;
  managedServerStatus?: () => Promise<ManagedServerStatus>;
  startManagedServer?: (
    nodeName: string,
    authority?: ManagedServerAuthorityInput,
  ) => Promise<ManagedServerStatus>;
  restartManagedServer?: (
    nodeName: string,
    authority: ManagedServerAuthorityInput,
  ) => Promise<ManagedServerStatus>;
  resetManagedServer?: (
    confirmation?: string,
    disposition?: HomeResetDisposition,
    fullReset?: boolean,
  ) => Promise<ManagedServerResetResult>;
  quitDesktop?: () => Promise<void>;
  validateManagedServerRoot?: (path: string) => Promise<string>;
  openManagedServerLoginItems?: () => Promise<void>;
  commitManagedServerAutoStart?: (
    nodeName: string,
  ) => Promise<ManagedServerStatus>;
  stopManagedServer?: (
    disableAutoStart: boolean,
  ) => Promise<ManagedServerStatus>;
  openDbExplorer?: () => Promise<string>;
  /** Opens a URL in the person's browser. The bridge strips the packaged
   *  build's own libraries and display backend from the environment the
   *  browser inherits, which the OS opener cannot. */
  openExternalUrl?: (url: string) => Promise<void>;
  setManagedServerAutoStart?: (
    enabled: boolean,
  ) => Promise<ManagedServerStatus>;
  setSelectedNode: (nodeDid: string | null) => Promise<void>;
  removePeer: (peerId: string) => Promise<DesktopClientSnapshot>;
  renamePeer: (peerId: string, label: string) => Promise<DesktopClientSnapshot>;
  fetchPeerStatus: (peerId: string) => Promise<unknown>;
  requestStatusEnrollment: (
    serverAddress: string,
  ) => Promise<EnrollmentRequestView>;
  listWorkspace: (subpath?: string | null) => Promise<WorkspaceListingView>;
  fetchRequestTimeline: (
    nodeDid: string,
    requestId: string,
  ) => Promise<RequestTimelineView>;
  explainToolSurface: (
    nodeDid: string,
    agentId: string,
  ) => Promise<ToolSurfaceExplanationView>;
  fetchNetworkStatus: () => Promise<NetworkStatusView>;
  fetchSessionSnapshot: (
    sessionId: string,
    nodeDid?: string | null,
    requestId?: string | null,
    timelinePage?: {
      limit?: number;
      beforeItemKey?: string | null;
    },
  ) => Promise<DesktopSessionSnapshot | null>;
  retrySessionHydration: (
    sessionId: string,
    nodeDid?: string | null,
  ) => Promise<void>;
  /** A stretch of a session's display rows by message number, without the
   *  session's request state. */
  fetchSessionRows: (request: {
    sessionId: string;
    nodeDid?: string | null;
    window: SessionRowsWindow;
    limit?: number;
  }) => Promise<SessionRowsView | null>;
  fetchSessionLiveDelta?: (request: {
    sessionId: string;
    nodeDid?: string | null;
    requestId: string;
    baseLiveCursor: string;
    baseContentByteLen: number;
    baseContentHash: string;
    baseReasoningByteLen: number;
    baseReasoningHash: string;
  }) => Promise<SessionLiveDeltaView | null>;
  sendChatMessage: (request: ChatSendRequest) => Promise<ChatSendResult>;
  steerPendingMessage: (
    request: PendingMessageSteerRequest,
  ) => Promise<PendingQueueEditResult>;
  editPendingQueue: (
    request: PendingQueueEditRequest,
  ) => Promise<PendingQueueEditResult>;
  listMailbox: () => Promise<MailboxItemView[]>;
  startMailboxRequest: (itemId: string) => Promise<MailboxItemView>;
  dismissMailboxItem: (itemId: string) => Promise<void>;
  forkSessionBeforeMessage: (
    request: SessionForkBeforeRequest,
  ) => Promise<SessionForkResult>;
  renameSession: (request: SessionRenameRequest) => Promise<void>;
  resendRequest: (
    requestId: string,
    nodeDid?: string,
  ) => Promise<RequestResendResult>;
  retryRequest: (
    requestId: string,
    nodeDid?: string,
  ) => Promise<ChatSendResult>;
  applyConfigComponents: (
    request: ConfigComponentsApplyRequest,
  ) => Promise<DesktopClientSnapshot>;
  patchConfigComponents: (
    request: ConfigComponentsPatchRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveNodeConfig: (
    request: NodeConfigSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  setDefaultAgent: (
    request: DefaultAgentSetRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveAgentConfig: (
    request: AgentSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveSkillConfig: (
    request: SkillSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteSkillConfig: (
    request: SkillDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteTaskConfig: (
    request: TaskDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteScheduleConfig: (
    request: ScheduleDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveEventSourceConfig: (
    request: EventSourceSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteEventSourceConfig: (
    request: EventSourceDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteTriggerConfig: (
    request: TriggerDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteBackendConfig: (
    request: BackendDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteInferenceProfileConfig: (
    request: InferenceProfileDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteToolsConfig: (
    request: ToolsDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteToolServiceConfig: (
    request: ToolServiceDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteAgentConfig: (
    request: AgentDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteContextConfig: (
    request: ContextDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveBackendConfig: (
    request: BackendSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  probeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  getInferenceSetupCatalog: () => Promise<InferenceSetupCatalog>;
  discoverInferenceModels: (
    request: InferenceDiscoveryRequest,
  ) => Promise<InferenceDiscoveryResult>;
  getInferenceModelRecommendation: (
    request: InferenceRecommendationRequest,
  ) => Promise<InferenceModelRecommendation>;
  getInferenceBackendRecommendation: (
    request: InferenceBackendRecommendationRequest,
  ) => Promise<InferenceModelRecommendation>;
  codexLogin: (
    nodeDid: string,
    provider?: string | null,
    label?: string | null,
  ) => Promise<CodexLoginResult>;
  cancelCodexLogin: () => Promise<void>;
  grokLogin: (
    nodeDid: string,
    provider?: string | null,
    label?: string | null,
  ) => Promise<GrokLoginResult>;
  cancelGrokLogin: () => Promise<void>;
  claudeLogin: (
    nodeDid: string,
    provider?: string | null,
    label?: string | null,
  ) => Promise<ClaudeLoginResult>;
  cancelClaudeLogin: () => Promise<void>;
  watchProviderLoginUrl?: (
    provider: OauthProvider,
    onUrl: (url: string) => void,
  ) => Promise<() => void>;
  fetchProviderRecovery?: (
    request: ProviderRecoveryRequest,
  ) => Promise<ProviderRecoveryPlan | null>;
  applyProviderRecovery?: (
    request: ProviderRecoveryApply,
  ) => Promise<ProviderRecoveryResult>;
  listProviderAccounts?: (nodeDid: string) => Promise<ProviderAccountView[]>;
  disconnectProviderAccount?: (
    nodeDid: string,
    credentialId: string,
  ) => Promise<void>;
  retrySaveProviderAccount?: (
    nodeDid: string,
    provider: string,
  ) => Promise<ProviderAccountView>;
  renameProviderAccount?: (
    nodeDid: string,
    credentialId: string,
    label: string,
  ) => Promise<void>;
  removeProviderAccount?: (
    nodeDid: string,
    credentialId: string,
  ) => Promise<void>;
  readProviderUsage?: (
    nodeDid: string,
    refresh: boolean,
    provider?: string | null,
  ) => Promise<AccountUsageView[]>;
  saveInferenceProfileConfig: (
    request: InferenceProfileSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveToolsConfig: (
    request: ToolsSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveToolServiceConfig: (
    request: ToolServiceSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  testToolService: (
    request: ToolServiceTestRequest,
  ) => Promise<ToolServiceTestResult>;
  saveTaskConfig: (request: TaskSaveRequest) => Promise<DesktopClientSnapshot>;
  saveScheduleConfig: (
    request: ScheduleSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  runSchedule: (request: ScheduleRunRequest) => Promise<TaskRunResult>;
  saveTriggerConfig: (
    request: TriggerSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  runTask: (request: TaskRunRequest) => Promise<TaskRunResult>;
  sessionProvenance: (
    request: DesktopSessionProvenanceRequest,
  ) => Promise<SessionProvenanceView>;
  listBackendsWithHealth: () => Promise<BackendHealth[]>;
  listMcpServicesWithHealth: () => Promise<MCPServiceHealthView[]>;
  probeMcpService: (
    nodeDid: string,
    serviceId: string,
  ) => Promise<McpServiceProbeResult>;
  fetchOperationsSnapshot: (
    request: DesktopOperationsSnapshotRequest,
  ) => Promise<DesktopOperationsSnapshot>;
  interruptRequest: (
    request: DesktopInterruptRequestRequest,
  ) => Promise<InterruptRequestResult>;
  backgroundTool: (request: DesktopBackgroundToolRequest) => Promise<boolean>;
  jobControl: (request: DesktopJobControlRequest) => Promise<JobControlOutcome>;
  listInstalledPacks: () => Promise<{ packs: InstalledPack[] }>;
  listPackPluginSlots: () => Promise<{
    plugins: PackPluginSlot[];
    profiles: PackSlotProfile[];
  }>;
  readPackAccount: () => Promise<{ account: { username: string } }>;
  searchPacks: (
    query: string,
    page: number,
  ) => Promise<{ packs: FoundPack[]; has_more: boolean }>;
  installPack: (request: PackInstallRequest) => Promise<void>;
  updatePack: (pack: string, edited: PackEditedChoice) => Promise<void>;
  removePack: (pack: string) => Promise<void>;
  bindPackPlugin: (plugin: string, profile: string | null) => Promise<void>;
  wirePack: (request: PackWireRequest) => Promise<PackWireResult>;
  listPackWireSources: () => Promise<PackWireSources>;
  signInToPackRegistry: (token: string) => Promise<void>;
  signOutOfPackRegistry: () => Promise<void>;
  listAllowedFolders: () => Promise<AllowedFolders>;
  addAllowedFolder: (
    path: string,
    access: AllowedFolderAccess,
  ) => Promise<AllowedFolders>;
  removeAllowedFolder: (path: string) => Promise<AllowedFolders>;
  subscribePluginApprovals: (token: string) => Promise<void>;
  unsubscribePluginApprovals: (token: string) => Promise<void>;
  listPendingPluginApprovals: () => Promise<{
    requests: PluginApprovalRequest[];
  }>;
  decidePluginApproval: (
    id: string,
    decision: PluginApprovalDecision,
  ) => Promise<void>;
  listUxPlugins: () => Promise<{ plugins: UxPluginListing[] }>;
  readUxPluginSource: (id: string) => Promise<UxPluginSource>;
  callPackPlugin: (
    request: PackPluginCallRequest,
  ) => Promise<{ output: unknown }>;
  callUxData: (request: UxDataCallRequest) => Promise<UxDataCallResult>;
  listUxDataSurfaces: (
    request: UxDataSurfacesRequest,
  ) => Promise<{ surfaces: unknown[] }>;
};

export type PackCommand =
  | "listInstalledPacks"
  | "listPackPluginSlots"
  | "readPackAccount"
  | "searchPacks"
  | "installPack"
  | "updatePack"
  | "removePack"
  | "bindPackPlugin"
  | "wirePack"
  | "listPackWireSources"
  | "signInToPackRegistry"
  | "signOutOfPackRegistry";

export type HostAccessCommand =
  | "listAllowedFolders"
  | "addAllowedFolder"
  | "removeAllowedFolder"
  | "subscribePluginApprovals"
  | "unsubscribePluginApprovals"
  | "listPendingPluginApprovals"
  | "decidePluginApproval";

export type UxPluginCommand =
  | "listUxPlugins"
  | "readUxPluginSource"
  | "callPackPlugin"
  | "callUxData"
  | "listUxDataSurfaces";

export type {
  HomeResetDisposition,
  ManagedServerResetResult,
  ManagedServerStatus,
};
