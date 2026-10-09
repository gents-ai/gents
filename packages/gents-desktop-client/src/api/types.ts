import type { OauthProvider } from "../transport.js";
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
import type { BackendUsageView } from "../generated/BackendUsageView.js";
import type { InferenceSetupCatalog } from "../generated/InferenceSetupCatalog.js";
import type { InferenceDiscoveryRequest } from "../generated/InferenceDiscoveryRequest.js";
import type { InferenceDiscoveryResult } from "../generated/InferenceDiscoveryResult.js";
import type { InferenceRecommendationRequest } from "../generated/InferenceRecommendationRequest.js";
import type { InferenceBackendRecommendationRequest } from "../generated/InferenceBackendRecommendationRequest.js";
import type { InferenceModelRecommendation } from "../generated/InferenceModelRecommendation.js";
import type {
  AgentConfigSaveRequest,
  DefaultBehaviorSetRequest,
  BackendDeleteRequest,
  BackendSaveRequest,
  BehaviorDeleteRequest,
  ContextDeleteRequest,
  BehaviorSaveRequest,
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
  TriggerDeleteRequest,
  TriggerSaveRequest,
  InferenceProbeResult,
  InferenceProfileDeleteRequest,
  InferenceProfileSaveRequest,
  InitSummary,
  InterruptRequestResult,
  MailboxItemView,
  MailboxQuestionAnswer,
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
} from "../types/packs.js";
import type {
  AllowedFolderAccess,
  AllowedFolders,
  PluginApprovalDecision,
  PluginApprovalRequest,
} from "../types/hostAccess.js";

export type ManagedServerAuthorityInput = {
  toolCeiling: ManagedServerToolCeiling;
  toolRoot: string | null;
};

export type DesktopApiAdapter = {
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
    agentName: string,
    authority?: ManagedServerAuthorityInput,
  ) => Promise<ManagedServerStatus>;
  restartManagedServer?: (
    agentName: string,
    authority: ManagedServerAuthorityInput,
  ) => Promise<ManagedServerStatus>;
  /** Omit `confirmation` to preview a home this version cannot open. */
  resetManagedServer?: (
    confirmation?: string,
    disposition?: HomeResetDisposition,
  ) => Promise<ManagedServerResetResult>;
  /** Quits the application, leaving all local state untouched. */
  quitDesktop?: () => Promise<void>;
  validateManagedServerRoot?: (path: string) => Promise<string>;
  openManagedServerLoginItems?: () => Promise<void>;
  commitManagedServerAutoStart?: (
    agentName: string,
  ) => Promise<ManagedServerStatus>;
  stopManagedServer?: (
    disableAutoStart: boolean,
  ) => Promise<ManagedServerStatus>;
  /** Opens the managed runtime's DB explorer window; resolves to its URL. */
  openDbExplorer?: () => Promise<string>;
  /** Opens a URL in the person's browser. The bridge strips the packaged
   *  build's own libraries and display backend from the environment the
   *  browser inherits, which the OS opener cannot. */
  openExternalUrl?: (url: string) => Promise<void>;
  setManagedServerAutoStart?: (enabled: boolean) => Promise<ManagedServerStatus>;
  setSelectedAgent: (agentDid: string | null) => Promise<void>;
  removePeer: (peerId: string) => Promise<DesktopClientSnapshot>;
  renamePeer: (peerId: string, label: string) => Promise<DesktopClientSnapshot>;
  fetchPeerStatus: (peerId: string) => Promise<unknown>;
  requestStatusEnrollment: (
    serverAddress: string,
  ) => Promise<EnrollmentRequestView>;
  listWorkspace: (subpath?: string | null) => Promise<WorkspaceListingView>;
  fetchRequestTimeline: (
    agentDid: string,
    requestId: string,
  ) => Promise<RequestTimelineView>;
  explainToolSurface: (
    agentDid: string,
    behaviorId: string,
  ) => Promise<ToolSurfaceExplanationView>;
  fetchNetworkStatus: () => Promise<NetworkStatusView>;
  fetchSessionSnapshot: (
    sessionId: string,
    agentDid?: string | null,
    requestId?: string | null,
    timelinePage?: {
      limit?: number;
      beforeItemKey?: string | null;
    },
  ) => Promise<DesktopSessionSnapshot | null>;
  retrySessionHydration: (
    sessionId: string,
    agentDid?: string | null,
  ) => Promise<void>;
  fetchSessionLiveDelta?: (request: {
    sessionId: string;
    agentDid?: string | null;
    requestId: string;
    baseLiveCursor: string;
    baseContentByteLen: number;
    baseContentHash: string;
    baseReasoningByteLen: number;
    baseReasoningHash: string;
  }) => Promise<SessionLiveDeltaView | null>;
  sendChatMessage: (request: {
    agentDid: string;
    behaviorId?: string | null;
    sessionId?: string | null;
    content: string;
    causedBySourceDocId?: string | null;
    /** An answer to the question on the `causedBySourceDocId` item; the
        bridge renders the content, so `content` is empty. */
    answer?: MailboxQuestionAnswer | null;
    /** The folder the user works in for this chat. */
    cwd?: string | null;
  }) => Promise<ChatSendResult>;
  listMailbox: () => Promise<MailboxItemView[]>;
  startMailboxRequest: (itemId: string) => Promise<MailboxItemView>;
  dismissMailboxItem: (itemId: string) => Promise<void>;
  renameSession: (request: SessionRenameRequest) => Promise<void>;
  resendRequest: (
    requestId: string,
    agentDid?: string,
  ) => Promise<RequestResendResult>;
  retryRequest: (
    requestId: string,
    agentDid?: string,
  ) => Promise<ChatSendResult>;
  applyConfigComponents: (
    request: ConfigComponentsApplyRequest,
  ) => Promise<DesktopClientSnapshot>;
  patchConfigComponents: (
    request: ConfigComponentsPatchRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveAgentConfig: (
    request: AgentConfigSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  setDefaultBehavior: (
    request: DefaultBehaviorSetRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveBehaviorConfig: (
    request: BehaviorSaveRequest,
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
  deleteBehaviorConfig: (
    request: BehaviorDeleteRequest,
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
    agentDid: string,
    provider?: string | null,
    label?: string | null,
  ) => Promise<CodexLoginResult>;
  cancelCodexLogin: () => Promise<void>;
  grokLogin: (
    agentDid: string,
    provider?: string | null,
    label?: string | null,
  ) => Promise<GrokLoginResult>;
  cancelGrokLogin: () => Promise<void>;
  claudeLogin: (
    agentDid: string,
    provider?: string | null,
    label?: string | null,
  ) => Promise<ClaudeLoginResult>;
  cancelClaudeLogin: () => Promise<void>;
  /** Calls `onUrl` with each sign-in URL the bridge sends while a
   *  provider's login runs; resolves to a function that stops watching. */
  watchProviderLoginUrl?: (
    provider: OauthProvider,
    onUrl: (url: string) => void,
  ) => Promise<() => void>;
  listProviderAccounts?: (agentDid: string) => Promise<ProviderAccountView[]>;
  disconnectProviderAccount?: (
    agentDid: string,
    credentialId: string,
  ) => Promise<void>;
  /** Saves a sign-in the bridge holds after a failed credential save,
   *  without repeating the browser login. `provider` is the credential kind. */
  retrySaveProviderAccount?: (
    agentDid: string,
    provider: string,
  ) => Promise<ProviderAccountView>;
  renameProviderAccount?: (
    agentDid: string,
    credentialId: string,
    label: string,
  ) => Promise<void>;
  /** Removes the account and the backends its sign-in created that no
   *  profile uses. */
  removeProviderAccount?: (
    agentDid: string,
    credentialId: string,
  ) => Promise<void>;
  /** Asks the runtime to read usage (skipping accounts read in the last
   *  five minutes), then returns each backend's stored usage. `refresh` is
   *  true for an explicit Refresh, the only read whose failure rejects.
   *  `provider` is a credential kind. */
  readProviderUsage?: (
    agentDid: string,
    refresh: boolean,
    provider?: string | null,
  ) => Promise<BackendUsageView[]>;
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
  probeMcpService: (serviceId: string) => Promise<McpServiceProbeResult>;
  fetchOperationsSnapshot: (
    request: DesktopOperationsSnapshotRequest,
  ) => Promise<DesktopOperationsSnapshot>;
  interruptRequest: (
    request: DesktopInterruptRequestRequest,
  ) => Promise<InterruptRequestResult>;
  listInstalledPacks: () => Promise<{ packs: InstalledPack[] }>;
  listPackPluginSlots: () => Promise<{
    plugins: PackPluginSlot[];
    profiles: PackSlotProfile[];
  }>;
  /** the registry account signed in, or a failure when none is */
  readPackAccount: () => Promise<{ account: { username: string } }>;
  searchPacks: (
    query: string,
    page: number,
  ) => Promise<{ packs: FoundPack[]; has_more: boolean }>;
  installPack: (request: PackInstallRequest) => Promise<void>;
  updatePack: (pack: string, edited: PackEditedChoice) => Promise<void>;
  removePack: (pack: string) => Promise<void>;
  bindPackPlugin: (plugin: string, profile: string | null) => Promise<void>;
  signInToPackRegistry: (token: string) => Promise<void>;
  signOutOfPackRegistry: () => Promise<void>;
  listAllowedFolders: () => Promise<AllowedFolders>;
  addAllowedFolder: (
    path: string,
    access: AllowedFolderAccess,
  ) => Promise<AllowedFolders>;
  removeAllowedFolder: (path: string) => Promise<AllowedFolders>;
  listPendingPluginApprovals: () => Promise<{ requests: PluginApprovalRequest[] }>;
  decidePluginApproval: (
    id: string,
    decision: PluginApprovalDecision,
  ) => Promise<void>;
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
  | "signInToPackRegistry"
  | "signOutOfPackRegistry";

export type HostAccessCommand =
  | "listAllowedFolders"
  | "addAllowedFolder"
  | "removeAllowedFolder"
  | "listPendingPluginApprovals"
  | "decidePluginApproval";

export type { HomeResetDisposition, ManagedServerResetResult, ManagedServerStatus };
