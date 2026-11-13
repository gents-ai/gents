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
import Proofs.Runtime.CanonicalOutput.Execution.AdmissionHandoffCases
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
import Proofs.Store.Identity.PublicationConformance
import Proofs.Session.Identity.DelegatedPublicationCases
import Proofs.Store.Identity.DelegatedFinality
import Proofs.Conformance.Identity.CallbackParent
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
import Proofs.Conformance.Deviations
import Proofs.Conformance.CoverageLedger
import Proofs.Conformance.Identity.Conformance
import Proofs.Store.Identity.SessionChild
import Proofs.Session.Request.RemoteDelivery
import Proofs.Session.Request.CrossActorAuthorization
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
import Proofs.Conformance.LspServerStart
import Proofs.Conformance.WorkspacePermissions

import Proofs.Conformance.InstallRepair

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
    ++ "\"compaction_canonical_projection_cases\":"
      ++ compactionCanonicalProjectionCasesJson ++ ","
    ++ "\"goal_link_cases\":"
      ++ Identity.SessionChild.goalLinkCasesJson ++ ","
    ++ "\"native_completion_cases\":" ++ Conformance.NativeCompletion.casesJson ++ ","
    ++ "\"native_claim_lifetime_cases\":" ++ Conformance.NativeClaimLifetime.casesJson ++ ","
    ++ "\"families\":" ++ (conformance_families%).compress ++ ","
    ++ "\"family_vocabularies\":" ++ (conformance_vocabularies%).compress ++ ","

def snapshotCoverageTailJson : String :=
  ""
    ++ "\"trigger_delivery_callback_visibility_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryCallbackVisibilityCasesJson ++ ","
    ++ "\"trigger_delivery_latest_admission_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryLatestAdmissionCasesJson ++ ","
    ++ "\"watcher_scoped_scan_cases\":"
      ++ EventDelivery.WatcherConformance.casesJson ++ ","
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ ",\"feature_surface_requirements\":"
      ++ featureSurfaceRequirementsJson
    ++ ",\"feature_matrix\":"
      ++ featureMatrixJson
    ++ ",\"identity_permission_cases\":"
      ++ Identity.Conformance.identityPermissionCasesJson
    ++ ",\"identity_household_cases\":"
      ++ Identity.Conformance.householdCasesJson
    ++ ",\"identity_task_session_publication_cases\":"
      ++ Identity.Conformance.taskSessionPublicationCasesJson
    ++ ",\"identity_client_session_reservation_cases\":"
      ++ Identity.Conformance.clientSessionReservationCasesJson
    ++ ",\"identity_atomic_client_request_cases\":"
      ++ Identity.SessionChild.atomicClientRequestCasesJson
    ++ ",\"identity_derivation_cases\":"
      ++ Identity.Conformance.derivationCasesJson
    ++ ",\"identity_contracts\":"
      ++ Identity.Conformance.identityContractsJson
    ++ "}"

def snapshotJsonTail : String :=
  snapshotPromptTailJson ++ snapshotCaptureTailJson ++ snapshotCoverageTailJson

def snapshotGoalCasesJson : String :=
  ""
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"tool_admission_handoff_cases\":"
      ++ CanonicalOutput.Execution.AdmissionHandoff.Cases.casesJson ++ ","
    ++ "\"delegated_publication_cases\":"
      ++ Identity.DelegatedPublication.Cases.casesJson ++ ","

def snapshotGoalMutationCasesJson : String :=
  ""
    ++ "\"goal_client_control_cases\":" ++ Conformance.GoalClientControlContracts.casesJson ++ ","
    ++ "\"goal_client_start_cases\":" ++ Conformance.GoalClientStartContracts.casesJson ++ ","
    ++ "\"goal_client_receipt_cases\":" ++ Conformance.GoalClientMutationContracts.receiptCasesJson ++ ","
    ++ "\"goal_client_delivery_cases\":" ++ Conformance.GoalClientDeliveryContracts.casesJson.compress ++ ","
    ++ "\"plugin_approval_cases\":" ++ Conformance.PluginApproval.casesJson.compress ++ ","
    ++ "\"goal_claimed_publication_cases\":"
      ++ Conformance.GoalClaimedPublicationContracts.casesJson ++ ","
    ++ "\"goal_operator_resume_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeCasesJson ++ ","
    ++ "\"goal_operator_resume_on_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeOnCasesJson ++ ","

def snapshotRequestCasesJson : String :=
  ""
    ++ "\"request_transition_cases\":"
      ++ jsonArray (requestTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"canonical_execution_gate_cases\":"
      ++ Conformance.ExecutionGateContracts.casesJson ++ ","
    ++ "\"session_input_edit_cases\":" ++ FoldQueueContracts.inputEditCasesJson ++ ","
    ++ "\"session_input_edit_client_observation_cases\":"
      ++ FoldQueueContracts.inputEditClientObservationCasesJson ++ ","
    ++ "\"canonical_dispatch_observation_cases\":"
      ++ Conformance.DispatchObservationContracts.casesJson ++ ","
    ++ "\"canonical_payload_presentation_cases\":"
      ++ Conformance.PayloadPresentationContracts.casesJson ++ ","
    ++ "\"process_transition_cases\":"
      ++ jsonArray (processTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"goal_create_cases\":" ++ goalCreateCasesJson ++ ","
    ++ "\"task_goal_publication_cases\":" ++ taskGoalPublicationCasesJson ++ ","

def snapshotSessionCasesJson : String :=
  ""
    ++ "\"request_input_cases\":" ++ requestInputCasesJson ++ ","
    ++ "\"frontend_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.frontendClientShellCaseCount ++ ","
    ++ "\"frontend_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.frontendClientShellCasesJson ++ ","

def snapshotIdentityCasesJson : String :=
  ""
    ++ "\"identity_publication_cases\":"
      ++ Identity.PublicationConformance.casesJson ++ ","
    ++ "\"identity_grant_recipient_cases\":"
      ++ Identity.GrantSetsConformance.recipientCasesJson ++ ","
    ++ "\"client_publication_protected_collection\":"
      ++ (Lean.toJson Identity.ClientAgentJournal.protectedCollection).compress ++ ","
    ++ "\"identity_parent_publication_cases\":"
      ++ Identity.PublicationConformance.parentCasesJson ++ ","

def snapshotClientCasesJson : String :=
  ""
    ++ "\"client_data_publication_cases\":"
      ++ Conformance.ClientDataPublication.casesJson ++ ","
    ++ "\"client_request_closure_cases\":"
      ++ Conformance.ClientRequestClosure.casesJson ++ ","
    ++ "\"client_session_turn_cases\":"
      ++ Conformance.ClientShellContracts.sessionTurnCasesJson ++ ","
    ++ "\"pack_operator_wire_cases\":" ++ PackInstallation.operatorWireCasesJson ++ ","
    ++ "\"runtime_reconcile_cases\":"
      ++ jsonArray (runtimeReconcileCases.map runtimeReconcileCaseJson) ++ ","

def snapshotRuntimeCasesJson : String :=
  ""
    ++ "\"persistence_failure_policy_cases\":"
      ++ jsonArray
        (persistenceFailurePolicyCases.map persistenceFailurePolicyCaseJson) ++ ","
    ++ "\"storage_observation_runtime_cases\":"
      ++ jsonArray
        (storageObservationRuntimeCases.map storageObservationRuntimeCaseJson) ++ ","
    ++ "\"native_filesystem_boundary_cases\":"
      ++ jsonArray
        (nativeFilesystemBoundaryCases.map nativeFilesystemBoundaryCaseJson) ++ ","
    ++ "\"managed_exec_tool_boundary_cases\":"
      ++ jsonArray
        (managedExecToolBoundaryCases.map managedExecToolBoundaryCaseJson) ++ ","
    ++ "\"managed_exec_liveness_cases\":"
      ++ jsonArray
        (managedExecLivenessCases.map managedExecLivenessCaseJson) ++ ","
    ++ "\"process_stop_cases\":"
      ++ jsonArray (processStopCases.map processStopCaseJson) ++ ","
    ++ "\"completion_retry_cases\":"
      ++ CompletionRetry.Contracts.casesJson ++ ","
    ++ "\"deviations\":"
      ++ deviationsJson ++ ","
    ++ "\"request_progress_cases\":"
      ++ jsonArray (requestProgressCases.map requestProgressCaseJson) ++ ","
    ++ "\"cross_actor_authorization_cases\":" ++ Request.CrossActorAuthorization.casesJson ++ ","

def snapshotRecoveryCasesJson : String :=
  ""
    ++ "\"queue_deadline_conformance_cases\":"
      ++ jsonArray
        (queueDeadlineConformanceCases.map queueDeadlineConformanceCaseJson) ++ ","
    ++ "\"recovery_sweep_cases\":"
      ++ jsonArray
        (Recovery.recoverySweepCases.map recoverySweepCaseJson) ++ ","
    ++ "\"recovery_sweep_ids\":"
      ++ jsonArray (Recovery.registeredRecoverySweepIds.map jsonString) ++ ","
    ++ "\"r6_backgrounding_cases\":"
      ++ jsonArray
        (r6BackgroundingCases.map r6BackgroundingCaseJson) ++ ","
    ++ "\"composed_invariant_witnesses\":"
      ++ jsonArray
        (composedInvariantWitnesses.map composedInvariantWitnessJson) ++ ","
    ++ "\"count_carrier_cases\":" ++ Conformance.CountCarrier.casesJson ++ ","
    ++ "\"schema_argument_repair_cases\":"
      ++ Conformance.SchemaArgumentRepair.casesJson ++ ","

def snapshotPluginCasesJson : String :=
  ""
    ++ "\"plugin_job_admission_cases\":" ++ Conformance.PluginJob.admissionCasesJson ++ ","
    ++ "\"plugin_job_adoption_cases\":" ++ Conformance.PluginJob.adoptionCasesJson ++ ","
    ++ "\"plugin_job_control_cases\":" ++ Conformance.PluginJob.controlCasesJson ++ ","
    ++ "\"causal_hop_default_max_request_hop\":"
      ++ Conformance.CausalHopContracts.defaultMaxRequestHopJson ++ ","
    ++ "\"workspace_path_capability_cases\":"
      ++ Conformance.WorkspacePathCapabilityContracts.casesJson ++ ","
    ++ "\"file_inventory_direct_reader_cases\":"
      ++ Conformance.FileInventoryContracts.directReaderCasesJson ++ ","
    ++ "\"file_inventory_fork_cases\":"
      ++ Conformance.FileInventoryContracts.forkCasesJson ++ ","
    ++ "\"file_inventory_source_projection_cases\":"
      ++ Conformance.FileInventoryContracts.sourceProjectionCasesJson ++ ","
    ++ "\"file_inventory_reader_cases\":"
      ++ Conformance.FileInventoryContracts.readerCasesJson ++ ","
    ++ "\"file_review_subscription_cases\":"
      ++ Conformance.FileReviewRouteContracts.subscriptionCasesJson ++ ","

def snapshotFileCasesJson : String :=
  ""
    ++ "\"callback_transition_cases\":" ++ callbackTransitionCasesJson ++ ","
    ++ "\"transcript_conformance_cases\":"
      ++ jsonArray
        (transcriptConformanceCases.map transcriptCaseJson) ++ ","
    ++ "\"canonical_output_projection_cases\":"
      ++ jsonArray
        (StreamingResponse.outputProjectionCases.map outputProjectionCaseJson) ++ ","
    ++ "\"reasoning_audit_cases\":" ++ jsonArray
      (StreamingResponse.ReasoningAudit.auditCases.map reasoningAuditCaseJson) ++ ","
    ++ "\"reasoning_signature_cases\":" ++ jsonArray
      (StreamingResponse.ReasoningAudit.signatureCases.map reasoningSignatureCaseJson) ++ ","
    ++ "\"auxiliary_output_cases\":" ++ jsonArray
      (CanonicalOutput.Execution.AuxiliaryCases.cases.map auxiliaryOutputCaseJson) ++ ","


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
