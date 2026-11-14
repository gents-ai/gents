import Proofs.Conformance.Identity.SessionChild
import Proofs.Conformance.RemoteDelivery
import Proofs.Conformance.StagedPermissionObjects
import Proofs.Conformance.SessionStopAdmission
import Proofs.Conformance.SessionInputEditMalformed
import Proofs.Conformance.StartupReadiness
import Proofs.Conformance.ProviderViewCallKeys
import Proofs.Conformance.ToolSurfaceArtifact
import Proofs.Conformance.WorkspaceFileConsent
import Proofs.Conformance.AuthoringFileMode
import Proofs.Conformance.DiscoveryFieldVisibility
import Proofs.Conformance.DatastoreProtection
import Proofs.Conformance.Identity.DelegatedPublicationRecovery
import Proofs.Conformance.SelfConfigStructured
import Proofs.Conformance.Recovery.PeriodicRegistry
import Proofs.Conformance.ProcessControlTerminal
import Proofs.Conformance.ToolCallTimeoutAfterSweep
import Proofs.Conformance.PluginJobCallerInterrupt
import Proofs.Conformance.BackgroundDiagnostics
import Proofs.Conformance.QueueEditSkillContext
import Proofs.Conformance.SessionStaleCompletion
import Proofs.Conformance.WorkspaceDirtyTrunk
import Proofs.Conformance.WorkspaceOverlayCwd
import Proofs.Conformance.SessionOrigin
import Proofs.Conformance.MailboxContent
import Proofs.Conformance.ManualRequest
import Proofs.Conformance.PendingRejection
import Proofs.Conformance.ManagedExecSpawn
import Proofs.Conformance.PluginBind
import Proofs.Conformance.OperatorProtection
import Proofs.Conformance.EnrollmentMemberRenewal
import Proofs.Conformance.ClaudeAuditStop
import Proofs.Conformance.ProviderInputCalibration
import Proofs.Conformance.UsageCacheRuntime
import Proofs.Conformance.RenderedCaptureBlockTurns
import Proofs.Conformance.EventGroupClock
import Proofs.Conformance.GoalResetResumeAccount
import Proofs.Conformance.GoalOperatorResumeProfiles
import Proofs.Conformance.Identity.SessionInputEdit
import Proofs.Conformance.Identity.ClientTransport
import Proofs.Conformance.PluginDataWrites
import Proofs.Conformance.PluginHostCalls
import Proofs.Conformance.CallbackAdmission
import Proofs.Conformance.GoalResetAccount
import Proofs.Conformance.WakeRedriveSweep
import Proofs.Conformance.ToolAdmission
import Proofs.Conformance.StartupPublication
import Proofs.Conformance.RequestRepairScope
import Proofs.Conformance.InterruptRequestDrain
import Proofs.Conformance.RootCandidates
import Proofs.Conformance.ToolRoot
import Proofs.Conformance.MCPHealthShutdown
import Proofs.Conformance.DatastoreWrite
import Proofs.Conformance.LspConsent
import Proofs.Conformance.ConfigurationDiscovery
import Proofs.Conformance.P2pAdministration
import Proofs.Conformance.Views
import Proofs.Conformance.AgentViews
import Proofs.Conformance.InferenceViews
import Proofs.Conformance.PluginGrantDeclaration
import Proofs.Conformance.EnrollmentRenewalNotices
import Proofs.Conformance.Scope
import Proofs.Conformance.EnrollmentAutoRenewal
import Proofs.Conformance.EnrollmentCore
import Proofs.Conformance.PairingSweepScheduling
import Proofs.Conformance.Identity.Operations
import Proofs.Conformance.ToolDispatch
import Proofs.Conformance.RequestAssembly
import Proofs.Conformance.UsageWrite
import Proofs.Conformance.SessionMessageKill
import Proofs.Conformance.InferenceObservationRegistry
import Proofs.Conformance.EagerStreaming
import Proofs.Conformance.BackendHealthRetention
import Proofs.Conformance.BackendProbeSchedule
import Proofs.Conformance.BackendHealthAdmission
import Proofs.Conformance.CaptureScope
import Proofs.Conformance.InferenceSlotAccounting
import Proofs.Conformance.FleetSlotAccounting
import Proofs.Conformance.Identity.AgentBaseline
import Proofs.Conformance.InferenceDroppedPermit
import Proofs.Conformance.EventSourceCursor
import Proofs.Conformance.Identity.AgentTargetAccess
import Proofs.Conformance.Identity.WorkspaceCompletion
import Proofs.Conformance.SessionObservation
import Proofs.Conformance.SourceBoundary
import Proofs.Conformance.LateDelivery
import Proofs.Conformance.WorkspaceProvision
import Proofs.Conformance.BaseFreezeRecovery
import Proofs.Conformance.Identity.ReceiverGrant
import Proofs.Conformance.Identity.WorkspaceRoot
import Proofs.Conformance.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Identity.WorkspaceCapability
import Proofs.Conformance.Identity.MailboxAccess
import Proofs.Conformance.Identity.SessionChildLocalSelfPublication
import Proofs.Conformance.Identity.NodeProvisioning
import Proofs.Conformance.Identity.WorkspaceRootAccess
import Proofs.Conformance.HomeResolution
import Proofs.Conformance.GraphqlName
import Proofs.Conformance.ClaudeRefresh
import Proofs.Conformance.Identity.BlockScope
import Proofs.Conformance.AccountResolver
import Proofs.Conformance.Identity.NetworkProtocol
import Proofs.Conformance.ClaudePromptAssembly
import Proofs.Conformance.Identity.WorkspaceCallbackSelection
import Proofs.Conformance.Identity.OwnerStaged
import Proofs.Conformance.Identity.DirectoryPublication
import Proofs.Conformance.Identity.ClientAgentJournal
import Proofs.Conformance.Identity.ClientAgentCreation
import Proofs.Conformance.Identity.SessionChildRecovery
import Proofs.Conformance.Identity.OwnerRegistration
import Proofs.Conformance.Identity.CreatedAgentAccess
import Proofs.Conformance.Identity.ApplicationPairingAccess
import Proofs.Conformance.ApplicationPairing
import Proofs.Conformance.Identity.EnrollmentGrantTraces
import Proofs.Conformance.Functions.Automation
import Proofs.Conformance.Functions.Clients
import Proofs.Conformance.Functions.Documents
import Proofs.Conformance.Functions.Inference
import Proofs.Conformance.Functions.Pack
import Proofs.Conformance.Functions.P2P
import Proofs.Conformance.Functions.Runtime
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.Functions.Tools
import Proofs.Conformance.Functions.Shared
import Proofs.Conformance.Functions.Store
import Proofs.Conformance.Pack
import Proofs.Conformance.StorageWriteGate
import Proofs.Conformance.StorageWriteGateAdmission
import Proofs.Conformance.StorageWriteGateStreamWaiter
import Proofs.Conformance.NativeCompletion
import Proofs.Conformance.NativeClaimLifetime
import Proofs.Conformance.UpdateHandoff
import Proofs.Conformance.AdmissionHandoff
import Proofs.Conformance.Contracts.Json.TerminalSelection
import Proofs.Conformance.WorkspaceCommandGrants
import Proofs.Conformance.StorageFailure
import Proofs.Conformance.ClientAgentReadiness
import Proofs.Conformance.RequestPatchPublicationCases
import Proofs.Conformance.ClientSessionDeliveryCases
import Proofs.Clients.ClientShell.OptimisticSend
import Proofs.Conformance.ClientChatReadinessCases
import Proofs.Conformance.ClientSyncHealthCases
import Proofs.Conformance.AccessReview
import Proofs.Conformance.BackendWake
import Proofs.Conformance.OperatorPaths
import Proofs.Conformance.WorkspaceCommandRoot
import Proofs.Conformance.WorkspaceFire
import Proofs.Conformance.CommandIsolation
import Proofs.Conformance.CompactionCheckpoint
import Proofs.Conformance.ClientDataPublicationCases
import Proofs.Conformance.ClientTelemetry
import Proofs.Conformance.InferenceObservation
import Proofs.Conformance.Identity.Publication
import Proofs.Conformance.Identity.GoalLinks
import Proofs.Session.Identity.DelegatedPublicationCases
import Proofs.Store.Identity.DelegatedFinality
import Proofs.Conformance.Identity.CallbackParent
import Proofs.Conformance.Identity.AgentIdentity
import Proofs.Conformance.Identity.SessionPublication
import Proofs.Conformance.Identity.DelegatedFinality
import Proofs.Conformance.Identity.EvalRecords
import Proofs.Conformance.Identity.OptimizationPromotion
import Proofs.Store.Identity.DirectoryPublication
import Proofs.Conformance.Identity.StartupReadiness
import Proofs.Store.Identity.NodeProvisioningConformance
import Proofs.Store.Identity.GrantSetsConformance
import Proofs.Store.Identity.ClientAgentCreation
import Proofs.Store.Identity.AgentBlockSigning
import Proofs.Conformance.Identity.Delegation
import Proofs.Conformance.Identity.AgentBlockSigning
import Proofs.Conformance.Identity.WorkspaceDatastoreWrite
import Proofs.ToolCalls.Identity.ClientAgentJournal
import Proofs.Conformance.Identity.NodeFileRequests
import Proofs.Conformance.RequestTransition
import Proofs.Conformance.TriggerDeliveryLatestAdmission
import Proofs.Conformance.WatcherScopedScan
import Proofs.Conformance.Identity.RevokeFollowsGrant
import Proofs.Conformance.PairingOwnedDiff
import Proofs.Conformance.PairingDataPlane
import Proofs.Conformance.PairingTick
import Proofs.Conformance.SchemaObligations
import Proofs.Conformance.DesiredStatePlan
import Proofs.Conformance.PluginStoreNotice
import Proofs.Conformance.NativeCompletionTrace
import Proofs.Store.Identity.MailboxAccess
import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Identity.PackPlacementReaders
import Proofs.Store.Identity.AgentTargetAccess
import Proofs.Conformance.NativeRoute
import Proofs.Session.Identity.SessionInputEdit
import Proofs.Automation.EventDelivery.WatcherConformance
import Proofs.Conformance.WatcherActors
import Proofs.Conformance.ClientInstallation
import Proofs.Conformance.RuntimeServiceReplacement
import Proofs.Conformance.ClientSettings
import Proofs.Conformance.ClientRequestClosureCases
import Proofs.Conformance.ClientRequestCacheIdentityCases
import Proofs.Conformance.SessionQueueOrderCases
import Proofs.Conformance.CountCarrier
import Proofs.Conformance.ConfigurationScope
import Proofs.Conformance.Initialization
import Proofs.Conformance.EnrollmentPendingRequests
import Proofs.Conformance.EnrollmentInvite
import Proofs.Conformance.WorkspacePermissionResolution
import Proofs.Conformance.OpenOutputShape
import Proofs.Conformance.EnrollmentCandidateName
import Proofs.Conformance.OperatorBaseFreeze
import Proofs.Conformance.LogicalOutputObligation
import Proofs.Conformance.InvalidToolProgress
import Proofs.Conformance.SchemaArgumentRepair
import Proofs.Conformance.RepeatedToolFailure
import Proofs.Conformance.ToolTimeouts
import Proofs.Conformance.PluginResources
import Proofs.Conformance.PluginJob
import Proofs.Conformance.PluginJobStep
import Proofs.Conformance.MailboxNotification
import Proofs.Conformance.MailboxReply
import Proofs.Conformance.MailboxQuestion
import Proofs.Conformance.MailboxHandoff
import Proofs.Conformance.MailboxSweep
import Proofs.Conformance.SessionReplyPreview
import Proofs.Conformance.ArtifactAuthority
import Proofs.Conformance.WorkspacePathCapability
import Proofs.Conformance.FileReviewRoutes
import Proofs.Conformance.FileInventory
import Proofs.Conformance.TaskHooks
import Proofs.Conformance.Hooks
import Proofs.Conformance.HookDecisions
import Proofs.Conformance.StoreEncryptionUpgrade
import Proofs.Conformance.TransactionReplay
import Proofs.Conformance.SubscriptionStream
import Proofs.Conformance.ReceiptRecovery
import Proofs.Conformance.HomeRetirement
import Proofs.Conformance.StoreKeyCustody
import Proofs.Conformance.BackgroundExecutionRegistry
import Proofs.Conformance.BackgroundWaitChange
import Proofs.Conformance.InvocationReply
import Proofs.Conformance.InterruptedDiagnostic
import Proofs.Conformance.HookInterrupt
import Proofs.Conformance.PluginJobDispatch
import Proofs.Conformance.Contracts.Json.Core
import Proofs.Conformance.Contracts.Json.Runtime
import Proofs.Conformance.Contracts.Json.CatalogRefresh
import Proofs.Conformance.Contracts.Json.ClaudePlatform
import Proofs.Conformance.Contracts.Json.PackInstallation
import Proofs.Conformance.Contracts.Json.Scheduling
import Proofs.Conformance.Contracts.Json.ToolExecution
import Proofs.Conformance.Contracts.Json.CommandPolicy
import Proofs.Conformance.Contracts.Json.ToolPolicy
import Proofs.Conformance.Contracts.Json.Lsp
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.Contracts.Json.BackgroundWork
import Proofs.Conformance.Contracts.Json.ComposedInvariants
import Proofs.Conformance.Contracts.Json.Workspace
import Proofs.Conformance.PairingReconcileSupervisor
import Proofs.Conformance.Contracts.Json.Callback
import Proofs.Conformance.Contracts.Json.SelfConfig
import Proofs.Conformance.Contracts.Json.SelfConfigAgent
import Proofs.Conformance.Contracts.Json.Goal
import Proofs.Conformance.GoalClaimedReadiness
import Proofs.Conformance.Contracts.Json.FileInventoryClosure
import Proofs.Conformance.Contracts.Json.PairingReconcile
import Proofs.Conformance.Contracts.Json.Enrollment
import Proofs.Conformance.TitleUsage
import Proofs.Conformance.ProviderUsage
import Proofs.Conformance.TitleAdmission
import Proofs.Conformance.TitleText
import Proofs.Conformance.Contracts.Json.PromptAssembly
import Proofs.Conformance.PromptAssemblyBudget
import Proofs.Conformance.PromptAssemblyRepair
import Proofs.Conformance.ProviderToolIdentity
import Proofs.Conformance.ReasoningDeltas
import Proofs.Conformance.AggregateBudget
import Proofs.Conformance.StateMachines
import Proofs.Conformance.HookPersistence
import Proofs.Conformance.Contracts.Json.RenderedCapture
import Proofs.Conformance.ContractCases.DurableReduction
import Proofs.Conformance.Contracts.Json.BackgroundWakeRows
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Proofs.Conformance.Contracts.Json.RequestInput
import Proofs.Conformance.Contracts.Json.AggregateBudget
import Proofs.Conformance.ContractCases.RollingCompaction
import Proofs.Conformance.ContractCases.ReductionEngine
import Proofs.Conformance.ContractCases.RenderedCapture
import Proofs.Conformance.ContractCases.CompactionProjectionJoin
import Proofs.Conformance.Contracts.Json.CompactionCanonicalProjection
import Proofs.Conformance.CompletionRetry.Contracts
import Proofs.Conformance.AccountRotation.Contracts
import Proofs.Conformance.AccountStore
import Proofs.Conformance.UsageRead
import Proofs.Conformance.Triggers.Contracts
import Proofs.Conformance.EventGroups
import Proofs.Conformance.ClientShell.Contracts
import Proofs.Conformance.ClientLiveDelta
import Proofs.Conformance.ApplyReconcile.ContractCases
import Proofs.Conformance.CoverageLedger
import Proofs.Conformance.Identity.Conformance
import Proofs.Store.Identity.SessionChild
import Proofs.Session.Request.RemoteDelivery
import Proofs.Session.Request.CrossActorAuthorization
import Proofs.Conformance.CrossActorAuthorization
import Proofs.Conformance.EventDelivery
import Proofs.Conformance.TriggerDelivery
import Proofs.Conformance.GoalOperatorResume
import Proofs.Conformance.GoalClaimedPublication
import Proofs.Conformance.GoalResetResume
import Proofs.Conformance.GoalWakeAncestry
import Proofs.Conformance.GoalWakeReceipt
import Proofs.Conformance.GoalWakeCoalescing
import Proofs.Conformance.GoalRequestAncestry
import Proofs.Conformance.GoalQueueAssignment
import Proofs.Conformance.GoalFirstRequestBinding
import Proofs.Conformance.GoalRequestHead
import Proofs.Conformance.NativeRefinement
import Proofs.Conformance.GoalRequestAssignment
import Proofs.Conformance.GoalDeliveryInput
import Proofs.Conformance.GoalToolContext
import Proofs.Conformance.GoalEdit
import Proofs.Conformance.SchemaArgumentRepairFamily
import Proofs.Conformance.ReasoningAudit
import Proofs.Conformance.CanonicalOutputProjection
import Proofs.Conformance.AuxiliaryOutput
import Proofs.Conformance.GoalClientControl
import Proofs.Conformance.GoalClientMutation
import Proofs.Conformance.GoalClientDelivery
import Proofs.Conformance.GoalClientStart
import Proofs.Conformance.GoalObservation
import Proofs.Conformance.GoalSessionObservation
import Proofs.Conformance.PluginApproval
import Proofs.Conformance.RequestExecutionLease
import Proofs.Conformance.InferenceRegistry
import Proofs.Conformance.RootAdmission
import Proofs.Conformance.Contracts.Json.ExecutionGate
import Proofs.Conformance.Contracts.Json.DispatchObservation
import Proofs.Conformance.Contracts.Json.InterruptQueue
import Proofs.Conformance.Contracts.Json.SessionControl
import Proofs.Conformance.Contracts.Json.FoldQueue
import Proofs.Conformance.Contracts.Json.WorkerCapacity
import Proofs.Conformance.Contracts.Json.CausalHop
import Proofs.Conformance.Contracts.Json.PayloadPresentation
import Proofs.Conformance.Eval
import Proofs.Conformance.Optimization
import Proofs.Documents.ScopeTemplates.Derivation
import Proofs.Conformance.ScopeCollectionRules
import Proofs.Conformance.Identity.SecretPublication
import Proofs.Conformance.Identity.HouseholdBPack
import Proofs.Conformance.Identity.HouseholdBInference
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Identity.HouseholdP2p
import Proofs.Conformance.Identity.HouseholdDocuments
import Proofs.Conformance.Identity.HouseholdAutomation
import Proofs.Conformance.Identity.HouseholdTools
import Proofs.Conformance.LspServerStart
import Proofs.Conformance.WorkspacePermissions

import Proofs.Conformance.InstallRepair
import Proofs.Conformance.RepairBackups

namespace Conformance.Contracts

open Conformance.ContractCases

/-- The snapshot's concatenation chains are bounded to keep model execution
within Lean's interpreter recursion limit. -/
def snapshotPromptTailJson : String :=
  ""

set_option maxHeartbeats 2000000 in
/-- Every family's schema elaborates in one list term, whose definitional
checks grow with the family count past the default heartbeat budget. -/
def snapshotCaptureTailJson : String :=
  ""
    ++ "\"families\":" ++ (conformance_families%).compress ++ ","
    ++ "\"family_vocabularies\":" ++ (conformance_vocabularies%).compress ++ ","

def snapshotCoverageTailJson : String :=
  ""
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ ",\"feature_surface_requirements\":"
      ++ featureSurfaceRequirementsJson
    ++ ",\"feature_matrix\":"
      ++ featureMatrixJson
    ++ ",\"identity_household_cases\":"
      ++ Identity.Conformance.householdCasesJson
    ++ "}"

def snapshotJsonTail : String :=
  snapshotPromptTailJson ++ snapshotCaptureTailJson ++ snapshotCoverageTailJson

def snapshotGoalCasesJson : String :=
  ""
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"delegated_publication_cases\":"
      ++ Identity.DelegatedPublication.Cases.casesJson ++ ","

def snapshotGoalMutationCasesJson : String :=
  ""
    ++ "\"goal_client_start_cases\":" ++ Conformance.GoalClientStartContracts.casesJson ++ ","

def snapshotRequestCasesJson : String :=
  ""
    ++ "\"canonical_execution_gate_cases\":"
      ++ Conformance.ExecutionGateContracts.casesJson ++ ","
    ++ "\"session_input_edit_cases\":" ++ FoldQueueContracts.inputEditCasesJson ++ ","
    ++ "\"canonical_dispatch_observation_cases\":"
      ++ Conformance.DispatchObservationContracts.casesJson ++ ","

def snapshotSessionCasesJson : String :=
  ""
    ++ "\"request_input_cases\":" ++ requestInputCasesJson ++ ","

def snapshotIdentityCasesJson : String :=
  ""
    ++ "\"identity_grant_recipient_cases\":"
      ++ Identity.GrantSetsConformance.recipientCasesJson ++ ","

def snapshotClientCasesJson : String :=
  ""

def snapshotRuntimeCasesJson : String :=
  ""
    ++ "\"managed_exec_tool_boundary_cases\":"
      ++ jsonArray
        (managedExecToolBoundaryCases.map managedExecToolBoundaryCaseJson) ++ ","
    ++ "\"process_stop_cases\":"
      ++ jsonArray (processStopCases.map processStopCaseJson) ++ ","
    ++ "\"completion_retry_cases\":"
      ++ CompletionRetry.Contracts.casesJson ++ ","

def snapshotRecoveryCasesJson : String :=
  ""
    ++ "\"recovery_sweep_cases\":"
      ++ jsonArray
        (Recovery.recoverySweepCases.map recoverySweepCaseJson) ++ ","
    ++ "\"recovery_sweep_ids\":"
      ++ jsonArray (Recovery.registeredRecoverySweepIds.map jsonString) ++ ","
    ++ "\"r6_backgrounding_cases\":"
      ++ jsonArray
        (r6BackgroundingCases.map r6BackgroundingCaseJson) ++ ","

def snapshotPluginCasesJson : String :=
  ""
    ++ "\"causal_hop_default_max_request_hop\":"
      ++ Conformance.CausalHopContracts.defaultMaxRequestHopJson ++ ","

def snapshotFileCasesJson : String :=
  ""
    ++ "\"transcript_conformance_cases\":"
      ++ jsonArray
        (transcriptConformanceCases.map transcriptCaseJson) ++ ","


def snapshotJson : String :=
  "{"
    ++ snapshotGoalCasesJson
    ++ snapshotGoalMutationCasesJson
    ++ snapshotRequestCasesJson
    ++ snapshotSessionCasesJson
    ++ snapshotIdentityCasesJson
    ++ snapshotClientCasesJson
    ++ snapshotRuntimeCasesJson
    ++ snapshotRecoveryCasesJson
    ++ snapshotPluginCasesJson
    ++ snapshotFileCasesJson
    ++ snapshotJsonTail

end Conformance.Contracts
