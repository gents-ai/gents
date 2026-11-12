import Proofs.Conformance.Views
import Proofs.Conformance.AgentViews
import Proofs.Conformance.InferenceViews
import Proofs.Conformance.PluginGrantDeclaration
import Proofs.Conformance.EnrollmentRenewalNotices
import Proofs.Conformance.ToolDispatch
import Proofs.Conformance.UsageWrite
import Proofs.Conformance.InferenceObservationRegistry
import Proofs.Conformance.EagerStreaming
import Proofs.Conformance.BackendHealthRetention
import Proofs.Conformance.InferenceDroppedPermit
import Proofs.Conformance.EventSourceCursor
import Proofs.Conformance.Identity.AgentTargetAccess
import Proofs.Conformance.Identity.WorkspaceCompletion
import Proofs.Conformance.Identity.ReceiverGrant
import Proofs.Conformance.Identity.WorkspaceRoot
import Proofs.Conformance.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Identity.MailboxAccess
import Proofs.Conformance.Identity.SessionChildLocalSelfPublication
import Proofs.Conformance.Identity.NodeProvisioning
import Proofs.Conformance.HomeResolution
import Proofs.Conformance.GraphqlName
import Proofs.Conformance.ClaudeRefresh
import Proofs.Conformance.Identity.BlockScope
import Proofs.Conformance.AccountResolver
import Proofs.Conformance.Identity.NetworkProtocol
import Proofs.Conformance.ClaudePromptAssembly
import Proofs.Conformance.Identity.WorkspaceCallbackSelection
import Proofs.Conformance.Identity.DirectoryPublication
import Proofs.Conformance.Identity.ClientAgentJournal
import Proofs.Conformance.Identity.ClientAgentCreation
import Proofs.Conformance.Identity.SessionChildRecovery
import Proofs.Conformance.Identity.OwnerRegistration
import Proofs.Conformance.Identity.CreatedAgentAccess
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
import Proofs.Conformance.RequestPatchPublicationCases
import Proofs.Conformance.ClientSessionDeliveryCases
import Proofs.Clients.ClientShell.OptimisticSend
import Proofs.Conformance.ClientChatReadinessCases
import Proofs.Conformance.ClientSyncHealthCases
import Proofs.Conformance.AccessReview
import Proofs.Conformance.BackendWake
import Proofs.Conformance.OperatorPaths
import Proofs.Conformance.WorkspaceCommandRoot
import Proofs.Conformance.CommandIsolation
import Proofs.Conformance.CompactionCheckpoint
import Proofs.Conformance.ClientDataPublicationCases
import Proofs.Conformance.ClientTelemetry
import Proofs.Conformance.InferenceObservation
import Proofs.Store.Identity.PublicationConformance
import Proofs.Session.Identity.DelegatedPublicationCases
import Proofs.Store.Identity.DelegatedFinality
import Proofs.Store.Identity.AutomationConformance
import Proofs.Store.Identity.OperationsConformance
import Proofs.Store.Identity.EvalRecords
import Proofs.Store.Identity.OptimizationPromotion
import Proofs.Store.Identity.DirectoryPublication
import Proofs.Store.Identity.StartupReadiness
import Proofs.Store.Identity.NodeProvisioningConformance
import Proofs.Store.Identity.GrantSetsConformance
import Proofs.Store.Identity.ClientAgentCreation
import Proofs.Store.Identity.AgentBlockSigning
import Proofs.ToolCalls.Identity.ClientAgentJournal
import Proofs.Conformance.Identity.NodeFileRequests
import Proofs.Conformance.Identity.RevokeFollowsGrant
import Proofs.Conformance.PairingOwnedDiff
import Proofs.Conformance.PairingDataPlane
import Proofs.Conformance.PairingTick
import Proofs.Conformance.SchemaObligations
import Proofs.Store.Identity.MailboxAccess
import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Store.Identity.PackPlacementReaders
import Proofs.Store.Identity.AgentTargetAccess
import Proofs.Automation.Triggers.NativeRoute
import Proofs.Session.Identity.SessionInputEdit
import Proofs.Automation.EventDelivery.WatcherConformance
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
import Proofs.Conformance.EnrollmentCandidateName
import Proofs.Conformance.OperatorBaseFreeze
import Proofs.Conformance.LogicalOutputObligation
import Proofs.Conformance.InvalidToolProgress
import Proofs.Conformance.SchemaArgumentRepair
import Proofs.Conformance.RepeatedToolFailure
import Proofs.Conformance.ToolTimeouts
import Proofs.Conformance.PluginResources
import Proofs.Conformance.PluginJob
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
import Proofs.Conformance.Contracts.Json.RenderedCapture
import Proofs.Conformance.Contracts.Json.DurableReduction
import Proofs.Conformance.Contracts.Json.BackgroundWakeRows
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Proofs.Conformance.Contracts.Json.RequestInput
import Proofs.Conformance.Contracts.Json.AggregateBudget
import Proofs.Conformance.ContractCases.RollingCompaction
import Proofs.Conformance.ContractCases.ReductionEngine
import Proofs.Conformance.ContractCases.RenderedCapture
import Proofs.Conformance.Contracts.Json.CompactionProjectionJoin
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

namespace Conformance.Contracts

open Conformance.ContractCases

def scopeRulesJson (rules : List ScopeTemplates.CollectionRule) : String :=
  jsonArray (rules.map fun rule =>
    "{\"collection\":" ++ jsonString rule.collection
      ++ ",\"field\":" ++ jsonString rule.field
      ++ ",\"source\":" ++ jsonString (match rule.source with
          | .localDid => "localDid" | .homeDid => "homeDid") ++ "}")

def nodeRouteCasesJson : String :=
  jsonArray (([ScopeTemplates.RouteDirection.clientToRuntime, .runtimeToClient]).flatMap fun direction =>
    ["did:key:owner", "did:key:quoted\"owner"].map fun owner =>
      let predicate := (ScopeTemplates.clientRouteFilters direction "did:key:reader" owner).find?
        (fun item => item.collection == "Node")
      "{\"direction\":" ++ jsonString (match direction with
        | .clientToRuntime => "client-to-runtime" | .runtimeToClient => "runtime-to-client") ++
      ",\"owner\":" ++ jsonString owner ++
      ",\"allowed\":" ++ boolJson ((ScopeTemplates.clientRouteCollections direction).contains "Node") ++
      ",\"node_did\":" ++ (predicate.bind fun item =>
        (item.clauses.find? fun clause => clause.field == "node_did").map (fun clause => jsonString clause.value)).getD "null" ++ "}")

def scopePredicateJson (predicate : ScopeTemplates.CollectionPredicate) : String :=
  "{\"collection\":" ++ jsonString predicate.collection ++
  ",\"clauses\":" ++ jsonArray (predicate.clauses.map fun clause =>
    "{\"field\":" ++ jsonString clause.field ++ ",\"operator\":" ++ jsonString clause.operator ++
    ",\"value\":" ++ jsonString clause.value ++ "}") ++ "}"

def scopeTemplateCasesJson : String :=
  jsonArray (ScopeTemplates.builtinCatalog.flatMap fun template =>
    [ScopeTemplates.RouteDirection.clientToRuntime, .runtimeToClient].map fun direction =>
      "{\"id\":" ++ jsonString template.id ++
      ",\"direction\":" ++ jsonString (match direction with
        | .clientToRuntime => "client-to-runtime" | .runtimeToClient => "runtime-to-client") ++
      ",\"collections\":" ++ jsonArray (template.collections.map jsonString) ++
      ",\"delivery\":" ++ jsonString (match template.delivery with | .push => "push") ++
      ",\"filters\":" ++ jsonArray ((ScopeTemplates.directionalScopeFilters template direction
        "did:key:reader" "did:key:owner").map scopePredicateJson) ++ "}")

/-- The snapshot's concatenation chains are bounded to keep model execution
within Lean's interpreter recursion limit. -/
def snapshotPromptTailJson : String :=
  ""
    ++ "\"prompt_assembly_retention_cases\":"
      ++ promptAssemblyRetentionCasesJson ++ ","
    ++ "\"prompt_assembly_compaction_trigger_cases\":"
      ++ promptAssemblyCompactionTriggerCasesJson ++ ","
    ++ "\"prompt_assembly_claude_map_cases\":"
      ++ promptAssemblyClaudeMapCasesJson ++ ","
    ++ "\"prompt_assembly_claude_body_cases\":"
      ++ promptAssemblyClaudeBodyCasesJson ++ ","
    ++ "\"prompt_assembly_claude_stream_cases\":"
      ++ promptAssemblyClaudeStreamCasesJson ++ ","
    ++ "\"prompt_assembly_claude_thinking_stream_cases\":"
      ++ promptAssemblyClaudeThinkingStreamCasesJson ++ ","
    ++ "\"prompt_assembly_claude_replay_cases\":"
      ++ promptAssemblyClaudeReplayCasesJson ++ ","
    ++ "\"prompt_assembly_claude_checkpoint_cases\":"
    ++ promptAssemblyClaudeCheckpointCasesJson ++ ","
    ++ "\"prompt_assembly_reasoning_suffix_cases\":"
    ++ promptAssemblyReasoningSuffixCasesJson ++ ","
    ++ "\"prompt_assembly_replay_shape_cases\":"
    ++ promptAssemblyReplayShapeCasesJson ++ ","
    ++ "\"prompt_assembly_replay_prefix_cases\":"
    ++ promptAssemblyReplayPrefixCasesJson ++ ","
    ++ "\"protected_replay_compaction_cases\":"
    ++ protectedReplayCompactionCasesJson ++ ","

def snapshotCaptureTailJson : String :=
  ""
    ++ "\"rendered_capture_cases\":"
      ++ renderedCaptureCasesJson ++ ","
    ++ "\"durable_reduction_cases\":"
      ++ durableReductionCasesJson ++ ","
    ++ "\"compaction_projection_join_cases\":"
      ++ compactionProjectionJoinCasesJson ++ ","
    ++ "\"compaction_canonical_projection_cases\":"
      ++ compactionCanonicalProjectionCasesJson ++ ","
    ++ "\"rendered_capture_key_cases\":"
      ++ renderedCaptureKeyCasesJson ++ ","
    ++ "\"auxiliary_session_child_binding_cases\":"
      ++ Identity.SessionChild.childScopeCasesJson ++ ","
    ++ "\"goal_link_cases\":"
      ++ Identity.SessionChild.goalLinkCasesJson ++ ","
    ++ "\"client_request_publication_cases\":"
      ++ Identity.SessionChild.clientRequestPublicationCasesJson ++ ","
    ++ "\"capture_scope_cases\":"
      ++ captureScopeCasesJson ++ ","
    ++ "\"capture_order_cases\":"
      ++ captureOrderCasesJson ++ ","
    ++ "\"aggregate_token_budget_cases\":"
      ++ aggregateTokenBudgetCasesJson ++ ","
    ++ "\"task_hook_admission_cases\":"
      ++ Conformance.TaskHooksContracts.admissionCasesJson ++ ","
    ++ "\"native_completion_cases\":" ++ Conformance.NativeCompletion.casesJson ++ ","
    ++ "\"native_claim_lifetime_cases\":" ++ Conformance.NativeClaimLifetime.casesJson ++ ","
    ++ "\"families\":" ++ (conformance_families%).compress ++ ","
    ++ "\"family_vocabularies\":" ++ (conformance_vocabularies%).compress ++ ","
    ++ "\"task_hook_run_cases\":"
      ++ Conformance.TaskHooksContracts.runCasesJson ++ ","
    ++ "\"task_hook_recovery_cases\":"
      ++ Conformance.TaskHooksContracts.recoveryCasesJson ++ ","
    ++ "\"compaction_reducer_cases\":"
      ++ jsonArray
        (Compaction.compactionReducerCases.map compactionReducerCaseJson) ++ ","
    ++ "\"compaction_cursor_cases\":"
      ++ jsonArray
        (Compaction.compactionCursorCases.map compactionCursorCaseJson) ++ ","
    ++ "\"mcp_health_cases\":"
      ++ jsonArray
        (Proofs.MCPHealth.transitionCases.map mcpHealthCaseJson) ++ ","
    ++ "\"backend_health_cases\":"
      ++ jsonArray
        (Proofs.BackendHealth.transitionCases.map backendHealthCaseJson) ++ ","
    ++ "\"backend_probe_schedule_cases\":"
      ++ jsonArray
        (Proofs.BackendHealth.scheduleCases.map backendProbeScheduleCaseJson) ++ ","

def snapshotCoverageTailJson : String :=
  ""
    ++ "\"follow_up_hooks\":"
      ++ followUpHooksJson ++ ","
    ++ "\"trigger_delivery_admission_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryAdmissionCasesJson ++ ","
    ++ "\"trigger_delivery_queue_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryQueueCasesJson ++ ","
    ++ "\"trigger_delivery_observed_claim_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryObservedClaimCasesJson ++ ","
    ++ "\"trigger_delivery_assignment_root_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryAssignmentRootCasesJson ++ ","
    ++ "\"trigger_delivery_outcome_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryOutcomeCasesJson ++ ","
    ++ "\"trigger_delivery_outcome_trace_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryOutcomeTraceCasesJson ++ ","
    ++ "\"trigger_delivery_retained_outcome_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryRetainedOutcomeCasesJson ++ ","
    ++ "\"trigger_delivery_cursor_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryCursorCasesJson ++ ","
    ++ "\"trigger_delivery_callback_cursor_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryCallbackCursorCasesJson ++ ","
    ++ "\"trigger_delivery_callback_visibility_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryCallbackVisibilityCasesJson ++ ","
    ++ "\"trigger_delivery_identity_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryIdentityCasesJson ++ ","
    ++ "\"trigger_delivery_session_cases\":" ++ Conformance.TriggerDelivery.triggerDeliverySessionCasesJson ++ ","
    ++ "\"trigger_delivery_session_config_cases\":" ++ Conformance.TriggerDelivery.triggerDeliverySessionConfigCasesJson ++ ","
    ++ "\"trigger_fire_budget_cases\":" ++ Conformance.TriggerDelivery.triggerFireBudgetCasesJson ++ ","
    ++ "\"trigger_fire_budget_config_cases\":" ++ Conformance.TriggerDelivery.triggerFireBudgetConfigCasesJson ++ ","
    ++ "\"trigger_fire_budget_default\":" ++ Conformance.TriggerDelivery.triggerFireBudgetDefaultJson ++ ","
    ++ "\"trigger_fire_budget_status\":" ++ Conformance.TriggerDelivery.triggerFireBudgetStatusJson ++ ","
    ++ "\"trigger_delivery_option_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryOptionCasesJson ++ ","
    ++ "\"trigger_delivery_latest_admission_cases\":" ++ Conformance.TriggerDelivery.triggerDeliveryLatestAdmissionCasesJson ++ ","
    ++ "\"trigger_delivery_self_session_cases\":" ++ Conformance.TriggerDelivery.triggerDeliverySelfSessionCasesJson ++ ","
    ++ "\"event_delivery_transition_cases\":"
      ++ Conformance.EventDelivery.transitionCasesJson ++ ","
    ++ "\"event_delivery_source_instances\":"
      ++ Conformance.EventDelivery.sourceInstancesJson ++ ","
    ++ "\"event_delivery_convergence_traces\":"
      ++ Conformance.EventDelivery.convergenceTracesJson ++ ","
    ++ "\"watcher_actor_cases\":"
      ++ EventDelivery.WatcherConformance.actorCasesJson ++ ","
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
    ++ "\"storage_failure_cases\":" ++ Conformance.StorageFailureContracts.casesJson ++ ","
    ++ "\"root_admission_cases\":"
      ++ Conformance.RootAdmissionContracts.casesJson ++ ","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"pairing_reconcile_cases\":" ++ pairingReconcileCasesJson ++ ","
    ++ "\"session_message_goal_input_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.messageInputCasesJson ++ ","
    ++ "\"goal_delivery_input_cases\":"
      ++ Conformance.GoalDeliveryInput.json ++ ","
    ++ "\"goal_first_request_binding_cases\":"
      ++ Conformance.GoalFirstRequestBinding.json ++ ","
    ++ "\"goal_continuation_assignment_cases\":"
      ++ Conformance.GoalFirstRequestBinding.continuationsJson ++ ","
    ++ "\"goal_input_binding_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.inputCasesJson ++ ","
    ++ "\"goal_fresh_session_input_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.freshSessionCasesJson ++ ","
    ++ "\"goal_request_assignment_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.casesJson ++ ","
    ++ "\"goal_wake_receipt_cases\":"
      ++ Conformance.GoalWakeReceiptContracts.casesJson ++ ","
    ++ "\"goal_wake_coalescing_cases\":"
      ++ Conformance.GoalWakeCoalescingContracts.casesJson ++ ","
    ++ "\"goal_wake_ancestry_cases\":"
      ++ Conformance.GoalWakeAncestryContracts.casesJson ++ ","
    ++ "\"goal_tool_context_cases\":"
      ++ Conformance.GoalToolContextContracts.casesJson ++ ","
    ++ "\"goal_request_scope_cases\":"
      ++ Conformance.GoalRequestAncestryContracts.scopeCasesJson ++ ","
    ++ "\"goal_request_ancestry_cases\":"
      ++ Conformance.GoalRequestAncestryContracts.casesJson ++ ","
    ++ "\"goal_queue_assignment_cases\":"
      ++ Conformance.GoalQueueAssignmentContracts.casesJson ++ ","
    ++ "\"durable_observation_cases\":"
      ++ Conformance.NativeRefinement.observationCasesJson ++ ","
    ++ "\"plugin_job_commit_kill_cases\":"
      ++ Conformance.PluginJob.commitKillCasesJson ++ ","
    ++ "\"client_process_claim_cases\":"
      ++ Conformance.NativeRefinement.clientProcessClaimCasesJson ++ ","
    ++ "\"client_process_receipt_cases\":"
      ++ Conformance.NativeRefinement.clientProcessReceiptCasesJson ++ ","
    ++ "\"client_control_grant_cases\":"
      ++ Conformance.NativeRefinement.clientControlGrantCasesJson ++ ","
    ++ "\"tool_admission_handoff_cases\":"
      ++ CanonicalOutput.Execution.AdmissionHandoff.Cases.casesJson ++ ","
    ++ "\"delegated_publication_cases\":"
      ++ Identity.DelegatedPublication.Cases.casesJson ++ ","
    ++ "\"delegated_finality_cases\":"
      ++ Identity.DelegatedFinality.casesJson ++ ","
    ++ "\"client_signed_control_cases\":"
      ++ Conformance.NativeRefinement.clientSignedControlCasesJson ++ ","
    ++ "\"process_stop_action_cases\":"
      ++ Conformance.NativeRefinement.processStopActionCasesJson ++ ","
    ++ "\"background_process_identity_cases\":"
      ++ Conformance.NativeRefinement.processIdentityCasesJson ++ ","
    ++ "\"plugin_target_resolution_cases\":"
      ++ Conformance.NativeRefinement.targetCasesJson ++ ","

def snapshotGoalMutationCasesJson : String :=
  ""
    ++ "\"goal_client_control_cases\":" ++ Conformance.GoalClientControlContracts.casesJson ++ ","
    ++ "\"goal_client_start_cases\":" ++ Conformance.GoalClientStartContracts.casesJson ++ ","
    ++ "\"goal_client_receipt_cases\":" ++ Conformance.GoalClientMutationContracts.receiptCasesJson ++ ","
    ++ "\"goal_client_delivery_cases\":" ++ Conformance.GoalClientDeliveryContracts.casesJson.compress ++ ","
    ++ "\"goal_observation_cases\":" ++ Conformance.GoalObservationContracts.casesJson ++ ","
    ++ "\"goal_session_observation_cases\":" ++ Conformance.GoalSessionObservationContracts.casesJson ++ ","
    ++ "\"goal_session_fold_cases\":" ++ Conformance.GoalSessionObservationContracts.foldCasesJson ++ ","
    ++ "\"workspace_permission_write_cases\":" ++ workspacePermissionWriteCasesJson ++ ","
    ++ "\"workspace_permission_cases\":" ++ workspacePermissionCasesJson ++ ","
    ++ "\"operator_path_cases\":" ++ Conformance.OperatorPaths.casesJson.compress ++ ","
    ++ "\"access_review_task_cases\":" ++ Conformance.AccessReview.taskCasesJson.compress ++ ","
    ++ "\"plugin_approval_cases\":" ++ Conformance.PluginApproval.casesJson.compress ++ ","
    ++ "\"workspace_command_grant_answers\":" ++ Conformance.WorkspaceCommandGrants.answersJson.compress ++ ","
    ++ "\"workspace_command_grant_lookups\":" ++ Conformance.WorkspaceCommandGrants.lookupsJson.compress ++ ","
    ++ "\"workspace_command_grant_revocations\":" ++ Conformance.WorkspaceCommandGrants.revocationsJson.compress ++ ","
    ++ "\"goal_objective_whitespace_codepoints\":"
      ++ Conformance.GoalEditContracts.whitespaceCodepointsJson ++ ","
    ++ "\"goal_claimed_publication_cases\":"
      ++ Conformance.GoalClaimedPublicationContracts.casesJson ++ ","
    ++ "\"goal_claimed_identity_cases\":"
      ++ Conformance.GoalClaimedPublicationContracts.identityCasesJson ++ ","
    ++ "\"goal_claimed_target_cases\":"
      ++ Conformance.GoalClaimedPublicationContracts.targetCasesJson ++ ","
    ++ "\"goal_resume_publication_cases\":" ++ Conformance.GoalOperatorResumeContracts.publicationCasesJson ++ ","
    ++ "\"goal_operator_resume_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeCasesJson ++ ","
    ++ "\"goal_operator_resume_on_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeOnCasesJson ++ ","
    ++ "\"goal_config_reactivation_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.configCasesJson ++ ","
    ++ "\"goal_reset_resume_cases\":"
      ++ Conformance.GoalResetResumeContracts.casesJson ++ ","

def snapshotRequestCasesJson : String :=
  ""
    ++ "\"request_transition_cases\":"
      ++ jsonArray (requestTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"provider_eof_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.providerEofCasesJson ++ ","
    ++ "\"request_execution_lease_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.leaseCasesJson ++ ","
    ++ "\"request_execution_lease_observed_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.observedLeaseCasesJson ++ ","
    ++ "\"request_execution_lease_trace_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.leaseTraceCasesJson ++ ","
    ++ "\"canonical_execution_gate_cases\":"
      ++ Conformance.ExecutionGateContracts.casesJson ++ ","
    ++ "\"interrupt_queue_cases\":"
      ++ Conformance.InterruptQueueContracts.casesJson ++ ","
    ++ "\"session_control_cases\":"
      ++ Conformance.SessionControlContracts.casesJson ++ ","
    ++ "\"session_input_edit_cases\":" ++ FoldQueueContracts.inputEditCasesJson ++ ","
    ++ "\"session_input_edit_client_observation_cases\":"
      ++ FoldQueueContracts.inputEditClientObservationCasesJson ++ ","
    ++ "\"queue_management_cases\":" ++ FoldQueueContracts.managementCasesJson ++ ","
    ++ "\"steering_publication_cases\":" ++ FoldQueueContracts.steeringPublicationCasesJson ++ ","
    ++ "\"fold_queue_cases\":"
      ++ Conformance.FoldQueueContracts.casesJson ++ ","
    ++ "\"fold_turn_input_cases\":"
      ++ Conformance.FoldQueueContracts.turnInputCasesJson ++ ","
    ++ "\"handover_fold_cases\":"
      ++ Conformance.FoldQueueContracts.handoverCasesJson ++ ","
    ++ "\"fold_publication_cases\":"
      ++ Conformance.FoldQueueContracts.publicationCasesJson ++ ","
    ++ "\"retry_selection_cases\":"
      ++ Conformance.FoldQueueContracts.retrySelectionCasesJson ++ ","
    ++ "\"canonical_dispatch_observation_cases\":"
      ++ Conformance.DispatchObservationContracts.casesJson ++ ","
    ++ "\"canonical_spawned_target_rejection_cases\":"
      ++ Conformance.DispatchObservationContracts.spawnedTargetCasesJson ++ ","
    ++ "\"canonical_worker_capacity_cases\":"
      ++ Conformance.WorkerCapacityContracts.casesJson ++ ","
    ++ "\"canonical_payload_presentation_cases\":"
      ++ Conformance.PayloadPresentationContracts.casesJson ++ ","
    ++ "\"terminal_diagnostic_presentation_cases\":"
      ++ Conformance.TerminalDiagnosticContracts.casesJson ++ ","
    ++ "\"terminal_diagnostic_replay_cases\":"
      ++ Conformance.TerminalDiagnosticReplayContracts.casesJson ++ ","
    ++ "\"process_transition_cases\":"
      ++ jsonArray (processTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"trigger_dispatch_case_count\":"
      ++ toString Conformance.TriggerContracts.triggerDispatchCaseCount ++ ","
    ++ "\"trigger_dispatch_cases\":"
      ++ Conformance.TriggerContracts.triggerDispatchCasesJson ++ ","
    ++ "\"event_group_case_count\":"
      ++ toString Conformance.EventGroupContracts.eventGroupCaseCount ++ ","
    ++ "\"event_group_cases\":"
      ++ Conformance.EventGroupContracts.eventGroupCasesJson ++ ","
    ++ "\"goal_decision_cases\":"
      ++ goalDecisionCasesJson ++ ","
    ++ "\"goal_readiness_gate_cases\":"
      ++ goalReadinessGateCasesJson ++ ","
    ++ "\"goal_claimed_readiness_cases\":"
      ++ Conformance.GoalClaimedReadiness.casesJson ++ ","
    ++ "\"goal_transition_cases\":"
      ++ goalTransitionCasesJson ++ ","
    ++ "\"goal_create_cases\":" ++ goalCreateCasesJson ++ ","
    ++ "\"task_goal_publication_cases\":" ++ taskGoalPublicationCasesJson ++ ","
    ++ "\"task_goal_recovery_cases\":" ++ taskGoalRecoveryCasesJson ++ ","
    ++ "\"goal_submission_cases\":" ++ goalSubmissionCasesJson ++ ","
    ++ "\"goal_backed_runtime_cases\":" ++ runtimeGoalPublicationCasesJson ++ ","
    ++ "\"goal_continuation_materialization_cases\":"
      ++ goalContinuationMaterializationCasesJson ++ ","
    ++ "\"lifecycle_hook_effect_cases\":"
      ++ Conformance.HooksContracts.effectCasesJson ++ ","
    ++ "\"lifecycle_hook_stage_cases\":"
      ++ Conformance.HooksContracts.stageCasesJson ++ ","
    ++ "\"lifecycle_hook_must_commit_cases\":"
      ++ Conformance.HooksContracts.mustCommitCasesJson ++ ","
    ++ "\"lifecycle_hook_rerun_cases\":"
      ++ Conformance.HooksContracts.rerunCasesJson ++ ","
    ++ "\"lifecycle_hook_goal_resume_cases\":"
      ++ Conformance.HooksContracts.goalResumeCasesJson ++ ","
    ++ "\"lifecycle_hook_combine_cases\":"
      ++ Conformance.HooksContracts.combineCasesJson ++ ","
    ++ "\"lifecycle_hook_gate_cases\":"
      ++ Conformance.HooksContracts.gateCasesJson ++ ","
    ++ "\"lifecycle_hook_queue_scope_cases\":"
      ++ Conformance.HooksContracts.queueScopeCasesJson ++ ","
    ++ "\"lifecycle_hook_delivery_cases\":"
      ++ Conformance.HooksContracts.deliveryCasesJson ++ ","
    ++ "\"lifecycle_hook_owed_delivery_cases\":"
      ++ Conformance.HooksContracts.owedCasesJson ++ ","
    ++ "\"lifecycle_hook_admit_cases\":"
      ++ Conformance.HooksContracts.admitCasesJson ++ ","
    ++ "\"lifecycle_hook_grant_cases\":"
      ++ Conformance.HooksContracts.grantCasesJson ++ ","
    ++ "\"lifecycle_hook_supersede_cases\":"
      ++ Conformance.HookDecisions.supersedeCasesJson ++ ","
    ++ "\"lifecycle_hook_goal_halt_cases\":"
      ++ Conformance.HookDecisions.goalHaltCasesJson ++ ","
    ++ "\"lifecycle_hook_history_cases\":"
      ++ Conformance.HookDecisions.historyCasesJson ++ ","

def snapshotSessionCasesJson : String :=
  ""
    ++ "\"file_inventory_closure_cases\":"
      ++ fileInventoryClosureCasesJson ++ ","
    ++ "\"enrollment_auto_renewal_cases\":" ++ autoRenewalCasesJson ++ ","
    ++ "\"enrollment_renewal_intent_cases\":" ++ renewalIntentCasesJson ++ ","
    ++ "\"enrollment_cases\":"
      ++ enrollmentCasesJson ++ ","
    ++ "\"enrollment_durable_projection_cases\":"
      ++ enrollmentDurableProjectionCasesJson ++ ","
    ++ "\"enrollment_pending_request_cases\":"
      ++ Conformance.EnrollmentPendingRequests.casesJson ++ ","
    ++ "\"enrollment_absent_peer_cases\":"
      ++ Conformance.EnrollmentPendingRequests.absentPeerCasesJson ++ ","
    ++ "\"client_chat_readiness_cases\":"
      ++ Conformance.ChatReadinessCases.casesJson ++ ","
    ++ "\"client_route_pairing_legs\":"
      ++ Conformance.ChatReadinessCases.legsJson ++ ","
    ++ "\"client_chat_readiness_causes\":"
      ++ Conformance.ChatReadinessCases.causesJson ++ ","
    ++ "\"client_sync_health_cases\":"
      ++ Conformance.ClientSyncHealthCases.casesJson ++ ","
    ++ "\"enrollment_route_readiness_cases\":"
      ++ Conformance.EnrollmentPendingRequests.routeCasesJson ++ ","
    ++ "\"enrollment_startup_cases\":"
      ++ Conformance.EnrollmentPendingRequests.startupCasesJson ++ ","
    ++ "\"enrollment_encoding_cases\":"
      ++ enrollmentEncodingCasesJson ++ ","
    ++ "\"enrollment_digest_cases\":"
      ++ enrollmentDigestCasesJson ++ ","
    ++ "\"discovery_scope_cases\":" ++ Conformance.ConfigurationScope.discoveryCasesJson ++ ","
    ++ "\"budget_rehydration_cases\":" ++ budgetRehydrationCasesJson ++ ","
    ++ "\"session_document_starter_verification_cases\":" ++ sessionDocumentStarterVerificationCasesJson ++ ","
    ++ "\"session_document_archive_cases\":" ++ sessionDocumentArchiveCasesJson ++ ","
    ++ "\"session_document_projection_cases\":" ++ sessionDocumentProjectionCasesJson ++ ","
    ++ "\"session_document_selection_cases\":" ++ sessionDocumentSelectionCasesJson ++ ","
    ++ "\"session_document_edit_anchor_cases\":" ++ sessionDocumentEditAnchorCasesJson ++ ","
    ++ "\"session_document_edit_fork_binding_cases\":" ++ sessionDocumentEditForkBindingCasesJson ++ ","
    ++ "\"session_document_fork_cases\":" ++ sessionDocumentForkCasesJson ++ ","
    ++ "\"plugin_job_opening_cases\":" ++ pluginJobOpeningCasesJson ++ ","
    ++ "\"session_reply_preview_cases\":" ++ Conformance.SessionReplyPreview.casesJson ++ ","
    ++ "\"background_wake_row_cases\":" ++ backgroundWakeRowsCasesJson ++ ","
    ++ "\"request_input_cases\":" ++ requestInputCasesJson ++ ","
    ++ "\"agent_request_admission_cases\":"
      ++ agentRequestAdmissionCasesJson ++ ","
    ++ "\"title_request_admission_cases\":"
      ++ titleRequestAdmissionCasesJson ++ ","
    ++ "\"title_request_purpose_wire_cases\":"
      ++ titlePurposeWireCasesJson ++ ","
    ++ "\"title_text_cases\":" ++ Conformance.TitleText.casesJson ++ ","
    ++ "\"title_usage_cases\":"
      ++ Conformance.TitleUsage.casesJson ++ ","
    ++ "\"provider_usage_cases\":"
      ++ Conformance.ProviderUsage.casesJson ++ ","
    ++ "\"title_admission_join_cases\":"
      ++ Conformance.TitleAdmission.casesJson ++ ","
    ++ "\"frontend_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.frontendClientShellCaseCount ++ ","
    ++ "\"frontend_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.frontendClientShellCasesJson ++ ","
    ++ "\"desktop_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.desktopClientShellCaseCount ++ ","
    ++ "\"desktop_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.desktopClientShellCasesJson ++ ","
    ++ "\"client_telemetry_cases\":" ++ Conformance.ClientTelemetry.casesJson ++ ","
    ++ "\"inference_observation_cases\":"
      ++ Conformance.InferenceObservation.casesJson ++ ","
    ++ "\"inference_visibility_cases\":"
      ++ Conformance.InferenceObservation.visibilityCasesJson ++ ","
    ++ "\"inference_route_cases\":"
      ++ Conformance.InferenceObservation.routeCasesJson ++ ","

def snapshotIdentityCasesJson : String :=
  ""
    ++ "\"identity_publication_cases\":"
      ++ Identity.PublicationConformance.casesJson ++ ","
    ++ "\"identity_grant_recipient_cases\":"
      ++ Identity.GrantSetsConformance.recipientCasesJson ++ ","
    ++ "\"identity_delegation_cases\":"
      ++ Identity.GrantSetsConformance.delegationCasesJson ++ ","
    ++ "\"identity_agent_baseline_cases\":"
      ++ Identity.GrantSetsConformance.baselineCasesJson ++ ","
    ++ "\"identity_agent_creator_cases\":"
      ++ Identity.NodeProvisioningConformance.agentCreatorCasesJson ++ ","
    ++ "\"identity_agent_block_signing_cases\":"
      ++ Identity.AgentBlockSigning.casesJson ++ ","
    ++ "\"mailbox_runtime_notice_case\":" ++ Identity.MailboxAccess.runtimeNoticeCasesJson ++ ","
    ++ "\"mailbox_claimed_recipient_cases\":" ++ Identity.MailboxAccess.claimedRecipientCasesJson ++ ","
    ++ "\"workspace_agent_selection_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceAgentSelectionCasesJson ++ ","
    ++ "\"workspace_liveness_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceLivenessCasesJson ++ ","
    ++ "\"workspace_child_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceChildCasesJson ++ ","
    ++ "\"workspace_quickstart_selection_cases\":" ++ Identity.WorkspaceAndFileRequests.quickstartSelectedAgentCasesJson ++ ","
    ++ "\"pack_placement_reader_cases\":" ++ Identity.PackPlacementReaders.casesJson ++ ","
    ++ "\"native_route_cases\":" ++ Triggers.NativeRoute.casesJson ++ ","
    ++ "\"workspace_datastore_write_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceDatastoreWriteCasesJson ++ ","
    ++ "\"workspace_root_access_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceRootAccessCasesJson ++ ","
    ++ "\"workspace_child_revocation_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceChildRevocationCasesJson ++ ","
    ++ "\"workspace_admission_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceAdmissionCasesJson ++ ","
    ++ "\"workspace_cleanup_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceCleanupCasesJson ++ ","
    ++ "\"workspace_publication_cases\":" ++ Identity.WorkspaceAndFileRequests.workspacePublicationCasesJson ++ ","
    ++ "\"workspace_quickstart_cases\":" ++ Identity.WorkspaceAndFileRequests.workspaceQuickstartCasesJson ++ ","
    ++ "\"mailbox_native_policy\":" ++ Identity.MailboxAccess.nativeCasesJson ++ ","
    ++ "\"session_input_edit_native_discovery_cases\":" ++ Identity.SessionInputEdit.discoveryCasesJson ++ ","
    ++ "\"session_input_edit_native_claim_cases\":" ++ Identity.SessionInputEdit.claimCasesJson ++ ","
    ++ "\"identity_private_journal_policy\":"
      ++ Identity.ClientAgentJournal.privatePolicyCasesJson ++ ","
    ++ "\"client_optimistic_send_cases\":"
      ++ ClientOptimisticSend.casesJson ++ ","
    ++ "\"identity_client_agent_recovery_cases\":"
      ++ Identity.ClientAgentJournal.recoveryCasesJson ++ ","
    ++ "\"client_publication_protected_collection\":"
      ++ (Lean.toJson Identity.ClientAgentJournal.protectedCollection).compress ++ ","
    ++ "\"identity_bootstrap_cases\":"
      ++ Identity.NodeProvisioningConformance.bootstrapCasesJson ++ ","
    ++ "\"identity_bootstrap_reservation_cases\":"
      ++ Identity.NodeProvisioningConformance.reservationCasesJson ++ ","
    ++ "\"identity_runtime_nac_cases\":"
      ++ Identity.NodeProvisioningConformance.runtimeNacCasesJson ++ ","
    ++ "\"identity_parent_publication_cases\":"
      ++ Identity.PublicationConformance.parentCasesJson ++ ","
    ++ "\"identity_eval_record_cases\":"
      ++ Identity.EvalRecords.casesJson ++ ","
    ++ "\"identity_optimization_promotion_cases\":"
      ++ Identity.OptimizationPromotion.casesJson ++ ","
    ++ "\"identity_readiness_preflight_cases\":"
      ++ Identity.OperationsConformance.preflightCasesJson ++ ","
    ++ "\"identity_secret_publication_cases\":"
      ++ Identity.OperationsConformance.secretPublicationCasesJson ++ ","
    ++ "\"identity_deferred_operation_cases\":"
      ++ Identity.OperationsConformance.deferredCasesJson ++ ","
    ++ "\"identity_pairing_partition_cases\":"
      ++ Identity.OperationsConformance.pairingPartitionCasesJson ++ ","
    ++ "\"identity_desired_publisher_cases\":"
      ++ Identity.OperationsConformance.desiredPublisherCasesJson ++ ","
    ++ "\"identity_pairing_reader_cases\":"
      ++ Identity.OperationsConformance.pairingReaderCasesJson ++ ","
    ++ "\"identity_pairing_reader_policy_cases\":"
      ++ Identity.OperationsConformance.pairingReaderPolicyCasesJson ++ ","
    ++ "\"identity_local_registry_writer_cases\":"
      ++ Identity.OperationsConformance.localRegistryWriterCasesJson ++ ","
    ++ "\"identity_discovery_policy_cases\":"
      ++ Identity.OperationsConformance.discoveryPolicyCasesJson ++ ","
    ++ "\"identity_client_transport_cases\":"
      ++ Identity.OperationsConformance.clientTransportCasesJson ++ ","
    ++ "\"identity_replica_grant_cases\":"
      ++ Identity.OperationsConformance.replicaGrantCasesJson ++ ","
    ++ "\"identity_enrollment_policy_cases\":"
      ++ Identity.OperationsConformance.enrollmentPolicyCasesJson ++ ","
    ++ "\"identity_readiness_recovery_cases\":"
      ++ Identity.OperationsConformance.recoveryCasesJson ++ ","
    ++ "\"identity_readiness_observation_cases\":"
      ++ Identity.OperationsConformance.observationCasesJson ++ ","
    ++ "\"identity_readiness_settled_cases\":"
      ++ Identity.OperationsConformance.settledCasesJson ++ ","
    ++ "\"identity_startup_readiness_cases\":"
      ++ Identity.StartupReadiness.casesJson ++ ","
    ++ "\"identity_callback_parent_actor_cases\":"
      ++ Identity.AutomationConformance.callbackParentCasesJson ++ ","

def snapshotClientCasesJson : String :=
  ""
    ++ "\"client_settings_cases\":"
      ++ clientSettingsCases.compress ++ ","
    ++ "\"client_data_publication_cases\":"
      ++ Conformance.ClientDataPublication.casesJson ++ ","
    ++ "\"client_request_patch_publication_cases\":"
      ++ Conformance.RequestPatchPublicationCases.casesJson ++ ","
    ++ "\"client_direct_session_read_cases\":"
      ++ Conformance.ClientSessionDelivery.directCasesJson ++ ","
    ++ "\"client_request_closure_cases\":"
      ++ Conformance.ClientRequestClosure.casesJson ++ ","
    ++ "\"client_request_cache_identity_cases\":"
      ++ Conformance.ClientRequestCacheIdentity.casesJson ++ ","
    ++ "\"client_session_queue_order_cases\":"
      ++ Conformance.SessionQueueOrder.casesJson ++ ","
    ++ "\"client_session_turn_cases\":"
      ++ Conformance.ClientShellContracts.sessionTurnCasesJson ++ ","
    ++ "\"request_lifecycle_operator_ui_cases\":"
      ++ Conformance.ClientShellContracts.requestLifecycleOperatorUiCasesJson ++ ","
    ++ "\"client_live_delta_cases\":"
      ++ Conformance.ClientLiveDeltaContracts.casesJson ++ ","
    ++ "\"plugin_reference_cases\":"
      ++ PackInstallation.casesJson ++ ","
    ++ "\"plugin_pin_cases\":" ++ PackInstallation.pinCasesJson ++ ","
    ++ "\"pack_wire_lock_cases\":" ++ PackInstallation.wireLockCasesJson ++ ","
    ++ "\"pack_wire_target_cases\":" ++ PackInstallation.wireTargetCasesJson ++ ","
    ++ "\"pack_operator_wire_cases\":" ++ PackInstallation.operatorWireCasesJson ++ ","
    ++ "\"pack_finality_cases\":" ++ PackInstallation.finalityCasesJson ++ ","
    ++ "\"hosted_plugin_cases\":" ++ PackInstallation.hostedPluginCasesJson ++ ","
    ++ "\"pack_eval_subject_cases\":" ++ PackInstallation.evalSubjectCasesJson ++ ","
    ++ "\"snapshot_demotion_cases\":"
      ++ snapshotDemotionCasesJson ++ ","
    ++ "\"startup_readiness_cases\":"
      ++ startupReadinessCasesJson ++ ","
    ++ "\"readiness_publication_cases\":"
      ++ readinessPublicationCasesJson ++ ","
    ++ "\"runtime_session_selection_cases\":" ++ sessionSelectionCasesJson ++ ","
    ++ "\"runtime_configuration_admission_cases\":" ++ configurationAdmissionCasesJson ++ ","
    ++ "\"runtime_reconcile_cases\":"
      ++ jsonArray (runtimeReconcileCases.map runtimeReconcileCaseJson) ++ ","
    ++ "\"client_agent_readiness_cases\":"
      ++ jsonArray (clientAgentReadinessCases.map clientAgentReadinessCaseJson) ++ ","
    ++ "\"eval_outcome_cases\":"
      ++ Conformance.Eval.evalOutcomeCasesJson ++ ","
    ++ "\"eval_document_capture_cases\":"
      ++ Conformance.Eval.evalDocumentCaptureCasesJson ++ ","
    ++ "\"optimization_cases\":"
      ++ Conformance.Optimization.optimizationCasesJson ++ ","
    ++ "\"tool_policy_cases\":"
      ++ toolPolicyCasesJson ++ ","
    ++ "\"application_write_cases\":" ++ applicationWriteCasesJson ++ ","
    ++ "\"field_read_cases\":" ++ fieldReadCasesJson ++ ","
    ++ "\"write_input_cases\":" ++ writeInputCasesJson ++ ","
    ++ "\"invocation_correlation_cases\":" ++ invocationCorrelationCasesJson ++ ","
    ++ "\"goal_capability_resolution_cases\":"
      ++ goalCapabilityResolutionCasesJson ++ ","
    ++ "\"lsp_action_cases\":"
      ++ lspActionCasesJson ++ ","
    ++ "\"self_config_field_tables\":"
      ++ selfConfigFieldTablesJson ++ ","
    ++ "\"self_config_resource_read_exposure_cases\":"
      ++ selfConfigResourceReadExposureCasesJson ++ ","
    ++ "\"self_config_scope_cases\":"
      ++ selfConfigScopeCasesJson ++ ","
    ++ "\"policy_resource_exposure_cases\":"
      ++ policyResourceExposureCasesJson ++ ","
    ++ "\"policy_grant_bound_cases\":"
      ++ policyGrantBoundCasesJson ++ ","
    ++ "\"policy_lockout_cases\":"
      ++ policyLockoutCasesJson ++ ","
    ++ "\"schema_publication_cases\":" ++ schemaPublicationCasesJson ++ ","
    ++ "\"self_config_cases\":"
      ++ selfConfigCasesJson ++ ","
    ++ "\"agent_decision_cases\":"
      ++ agentDecisionCasesJson ++ ","
    ++ "\"agent_materialization_cases\":"
      ++ agentMaterializationCasesJson ++ ","
    ++ "\"sibling_tools_cases\":"
      ++ siblingToolsCasesJson ++ ","
    ++ "\"self_config_selection_cases\":"
      ++ selfConfigSelectionCasesJson ++ ","
    ++ "\"inference_slot_accounting_cases\":"
      ++ jsonArray (inferenceSlotAccountingCases.map inferenceSlotAccountingCaseJson) ++ ","
    ++ "\"fleet_slot_accounting_cases\":"
      ++ jsonArray (fleetSlotAccountingCases.map fleetSlotAccountingCaseJson) ++ ","

def snapshotRuntimeCasesJson : String :=
  ""
    ++ "\"persistence_failure_policy_cases\":"
      ++ jsonArray
        (persistenceFailurePolicyCases.map persistenceFailurePolicyCaseJson) ++ ","
    ++ "\"storage_observation_runtime_cases\":"
      ++ jsonArray
        (storageObservationRuntimeCases.map storageObservationRuntimeCaseJson) ++ ","
    ++ "\"backend_health_admission_cases\":"
      ++ jsonArray
        (backendHealthAdmissionCases.map backendHealthAdmissionCaseJson) ++ ","
    ++ "\"native_filesystem_boundary_cases\":"
      ++ jsonArray
        (nativeFilesystemBoundaryCases.map nativeFilesystemBoundaryCaseJson) ++ ","
    ++ "\"managed_exec_tool_boundary_cases\":"
      ++ jsonArray
        (managedExecToolBoundaryCases.map managedExecToolBoundaryCaseJson) ++ ","
    ++ "\"pairing_reconcile_shutdown_boundary_cases\":"
      ++ jsonArray
        (pairingReconcileShutdownBoundaryCases.map
          pairingReconcileShutdownBoundaryCaseJson) ++ ","
    ++ "\"pairing_reconcile_sweep_retry_boundary_cases\":"
      ++ jsonArray
        (pairingReconcileSweepRetryBoundaryCases.map
          pairingReconcileSweepRetryBoundaryCaseJson) ++ ","
    ++ "\"pairing_reconcile_sweep_scheduling_cases\":"
      ++ jsonArray
        (pairingReconcileSweepSchedulingCases.map
          pairingReconcileSweepSchedulingCaseJson) ++ ","
    ++ "\"managed_exec_liveness_cases\":"
      ++ jsonArray
        (managedExecLivenessCases.map managedExecLivenessCaseJson) ++ ","
    ++ "\"process_stop_cases\":"
      ++ jsonArray (processStopCases.map processStopCaseJson) ++ ","
    ++ "\"tool_preflight_cases\":"
      ++ jsonArray (ToolExecution.preflightCases.map toolPreflightCaseJson) ++ ","
    ++ "\"tool_retry_cases\":"
      ++ jsonArray (ToolExecution.retryCases.map toolRetryCaseJson) ++ ","
    ++ "\"backend_wake_cases\":" ++ Conformance.BackendWake.casesJson ++ ","
    ++ "\"backend_capture_scope_cases\":" ++ Conformance.BackendWake.captureScopeCasesJson ++ ","
    ++ "\"backend_health_wake_cases\":" ++ Conformance.BackendWake.healthCasesJson ++ ","
    ++ "\"backend_generation_mark_cases\":" ++ Conformance.BackendWake.generationCasesJson ++ ","
    ++ "\"backend_resume_admission_cases\":" ++ Conformance.BackendWake.admissionCasesJson ++ ","
    ++ "\"backend_repark_cases\":" ++ Conformance.BackendWake.reparkCasesJson ++ ","
    ++ "\"backend_parked_finalize_cases\":" ++ Conformance.BackendWake.parkedFinalizeCasesJson ++ ","
    ++ "\"backend_resumed_turn_cases\":" ++ Conformance.BackendWake.resumedTurnCasesJson ++ ","
    ++ "\"completion_retry_cases\":"
      ++ CompletionRetry.Contracts.casesJson ++ ","
    ++ "\"boundaries\":"
      ++ boundariesJson ++ ","
    ++ "\"deviations\":"
      ++ deviationsJson ++ ","
    ++ "\"artifact_mode_meet_cases\":" ++ artifactModeMeetCasesJson ++ ","
    ++ "\"artifact_admission_cases\":" ++ artifactAdmissionCasesJson ++ ","
    ++ "\"artifact_spawn_cases\":" ++ artifactSpawnCasesJson ++ ","
    ++ "\"command_policy_cases\":"
      ++ jsonArray (CommandPolicy.commandPolicyCases.map commandPolicyCaseJson) ++ ","
    ++ "\"command_isolation_cases\":" ++ Conformance.CommandIsolation.casesJson.compress ++ ","
    ++ "\"command_env_cases\":"
      ++ jsonArray (CommandPolicy.commandEnvCases.map commandEnvCaseJson) ++ ","
    ++ "\"live_overlay_cases\":"
      ++ jsonArray (liveOverlayCases.map liveOverlayCaseJson) ++ ","
    ++ "\"request_progress_cases\":"
      ++ jsonArray (requestProgressCases.map requestProgressCaseJson) ++ ","
    ++ "\"pending_user_turn_cases\":"
      ++ jsonArray (pendingUserTurnCases.map pendingUserTurnCaseJson) ++ ","
    ++ "\"queued_steering_trace_cases\":"
      ++ jsonArray (QueuedSteering.traceObservations.map queuedSteeringTraceJson) ++ ","
    ++ "\"queued_steering_guard_cases\":"
      ++ jsonArray (QueuedSteering.guardObservations.map queuedSteeringGuardJson) ++ ","
    ++ "\"agent_delivery_cases\":"
      ++ jsonArray (QueuedSteering.agentDeliveryCases.map agentDeliveryCaseJson) ++ ","
    ++ "\"remote_agent_delivery_cases\":" ++ Request.RemoteDelivery.casesJson ++ ","
    ++ "\"remote_agent_delivery_recovery_cases\":" ++ Request.RemoteDelivery.recoveryCasesJson ++ ","
    ++ "\"remote_agent_delivery_claim_cases\":" ++ Request.RemoteDelivery.claimCasesJson ++ ","
    ++ "\"cross_actor_authorization_cases\":" ++ Request.CrossActorAuthorization.casesJson ++ ","
    ++ "\"remote_agent_delivery_response_cases\":" ++ Request.RemoteDelivery.responseCasesJson ++ ","
    ++ "\"interrupt_delivery_cases\":"
      ++ jsonArray (QueuedSteering.interruptDeliveryCases.map interruptDeliveryCaseJson) ++ ","

def snapshotRecoveryCasesJson : String :=
  ""
    ++ "\"agent_steer_disposition_cases\":"
      ++ jsonArray (QueuedSteering.agentSteerDispositionCases.map agentSteerDispositionCaseJson) ++ ","
    ++ "\"agent_steer_target_cases\":"
      ++ jsonArray (QueuedSteering.agentSteerTargetCases.map agentSteerTargetCaseJson) ++ ","
    ++ "\"agent_steering_boundary_cases\":"
      ++ jsonArray (QueuedSteering.boundaryCases.map boundaryCaseJson) ++ ","
    ++ "\"agent_steer_reachability_cases\":"
      ++ jsonArray (QueuedSteering.agentSteerReachabilityCases.map agentSteerReachabilityCaseJson) ++ ","
    ++ "\"agent_folded_binding_cases\":"
      ++ jsonArray (QueuedSteering.foldedBindingCases.map foldedBindingCaseJson) ++ ","
    ++ "\"agent_message_outcome_cases\":"
      ++ jsonArray (QueuedSteering.agentMessageOutcomeCases.map agentMessageOutcomeCaseJson) ++ ","
    ++ "\"queue_deadline_conformance_cases\":"
      ++ jsonArray
        (queueDeadlineConformanceCases.map queueDeadlineConformanceCaseJson) ++ ","
    ++ "\"recovery_sweep_cases\":"
      ++ jsonArray
        (Recovery.recoverySweepCases.map recoverySweepCaseJson) ++ ","
    ++ "\"recovery_sweep_ids\":"
      ++ jsonArray (Recovery.registeredRecoverySweepIds.map jsonString) ++ ","
    ++ "\"session_message_kill_cases\":"
      ++ jsonArray (Recovery.killCases.map fun (observation, action) =>
        "{\"observation\":" ++ jsonString observation.toContract
          ++ ",\"action\":" ++ jsonString action.toContract ++ "}") ++ ","
    ++ "\"session_message_kill_observation_cases\":"
      ++ jsonArray (Recovery.killObservationCases.map fun (caused, observation) =>
        "{\"caused\":" ++ (match caused with
          | none => "null"
          | some facts => "{\"terminal\":" ++ toString facts.terminal
              ++ ",\"this_runtime\":" ++ toString facts.thisRuntime ++ "}")
          ++ ",\"observation\":" ++ jsonString observation.toContract ++ "}") ++ ","
    ++ "\"restart_disposition_cases\":"
      ++ jsonArray
        (Recovery.restartDispositionCases.map restartDispositionCaseJson) ++ ","
    ++ "\"r4c_background_work_cases\":"
      ++ jsonArray r4cBackgroundWorkCasesJson ++ ","
    ++ "\"tool_output_paging_cases\":"
      ++ jsonArray
        (toolOutputPagingCases.map toolOutputPagingCaseJson) ++ ","
    ++ "\"interrupt_disposition_cases\":"
      ++ jsonArray
        (interruptDispositionCases.map interruptDispositionCaseJson) ++ ","
    ++ "\"r6_backgrounding_cases\":"
      ++ jsonArray
        (r6BackgroundingCases.map r6BackgroundingCaseJson) ++ ","
    ++ "\"composed_invariant_witnesses\":"
      ++ jsonArray
        (composedInvariantWitnesses.map composedInvariantWitnessJson) ++ ","
    ++ "\"count_carrier_cases\":" ++ Conformance.CountCarrier.casesJson ++ ","
    ++ "\"logical_output_obligation_cases\":"
      ++ Conformance.LogicalOutputObligationContracts.casesJson ++ ","
    ++ "\"mailbox_notification_cases\":"
      ++ Conformance.MailboxNotificationContracts.casesJson ++ ","
    ++ "\"mailbox_reply_cases\":"
      ++ Conformance.MailboxReplyContracts.casesJson ++ ","
    ++ "\"mailbox_question_cases\":"
      ++ Conformance.MailboxQuestionContracts.casesJson ++ ","
    ++ "\"mailbox_handoff_cases\":"
      ++ Conformance.MailboxHandoffContracts.casesJson ++ ","
    ++ "\"mailbox_sweep_cases\":"
      ++ Conformance.MailboxSweepContracts.casesJson ++ ","
    ++ "\"schema_argument_repair_cases\":"
      ++ Conformance.SchemaArgumentRepair.casesJson ++ ","
    ++ "\"invalid_tool_progress_cases\":"
      ++ Conformance.InvalidToolProgressContracts.casesJson ++ ","
    ++ "\"repeated_tool_failure_cases\":"
      ++ Conformance.RepeatedToolFailureContracts.casesJson ++ ","

def snapshotPluginCasesJson : String :=
  ""
    ++ "\"plugin_resource_consent_cases\":"
      ++ Conformance.PluginResources.pluginResourceConsentCasesJson ++ ","
    ++ "\"plugin_resource_budget_cases\":"
      ++ Conformance.PluginResources.pluginResourceBudgetCasesJson ++ ","
    ++ "\"plugin_resource_publication_cases\":"
      ++ Conformance.PluginResources.pluginResourcePublicationCasesJson ++ ","
    ++ "\"plugin_resource_tool_call_cases\":"
      ++ Conformance.PluginResources.pluginResourceToolCallCasesJson ++ ","
    ++ "\"plugin_resource_child_admission_cases\":"
      ++ Conformance.PluginResources.pluginResourceChildAdmissionCasesJson ++ ","
    ++ "\"plugin_resource_tool_effect_cases\":"
      ++ Conformance.PluginResources.pluginResourceToolEffectCasesJson ++ ","
    ++ "\"plugin_resource_tool_effect_receipt_cases\":"
      ++ Conformance.PluginResources.pluginResourceToolEffectReceiptCasesJson ++ ","
    ++ "\"plugin_resource_tool_effect_terminal_cases\":"
      ++ Conformance.PluginResources.pluginResourceToolEffectTerminalCasesJson ++ ","
    ++ "\"plugin_resource_tool_effect_accounting_cases\":"
      ++ Conformance.PluginResources.pluginResourceToolEffectAccountingCasesJson ++ ","
    ++ "\"plugin_resource_model_slot_cases\":"
      ++ Conformance.PluginResources.pluginResourceModelSlotCasesJson ++ ","
    ++ "\"plugin_resource_network_cases\":"
      ++ Conformance.PluginResources.pluginResourceNetworkCasesJson ++ ","
    ++ "\"plugin_resource_call_access_cases\":"
      ++ Conformance.PluginResources.pluginResourceCallAccessCasesJson ++ ","
    ++ "\"plugin_resource_invocation_cases\":"
      ++ Conformance.PluginResources.pluginResourceInvocationCasesJson ++ ","
    ++ "\"plugin_resource_receipt_cases\":"
      ++ Conformance.PluginResources.pluginResourceReceiptCasesJson ++ ","
    ++ "\"plugin_resource_scratch_cases\":"
      ++ Conformance.PluginResources.pluginResourceScratchCasesJson ++ ","
    ++ "\"plugin_resource_host_import_cases\":"
      ++ Conformance.PluginResources.pluginResourceHostImportCasesJson ++ ","
    ++ "\"plugin_data_cases\":" ++ Conformance.PluginResources.dataCasesJson ++ ","
    ++ "\"plugin_calls_route_cases\":" ++ Conformance.PluginResources.routeCasesJson ++ ","
    ++ "\"plugin_job_admission_cases\":" ++ Conformance.PluginJob.admissionCasesJson ++ ","
    ++ "\"plugin_job_adoption_cases\":" ++ Conformance.PluginJob.adoptionCasesJson ++ ","
    ++ "\"plugin_job_key_cases\":" ++ Conformance.PluginJob.keyCasesJson ++ ","
    ++ "\"plugin_job_control_cases\":" ++ Conformance.PluginJob.controlCasesJson ++ ","
    ++ "\"plugin_job_authority_cases\":" ++ Conformance.PluginJob.authorityCasesJson ++ ","
    ++ "\"plugin_job_wire_cases\":" ++ Conformance.PluginJob.wireCasesJson ++ ","
    ++ "\"plugin_job_lineage_cases\":" ++ Conformance.PluginJob.lineageCasesJson ++ ","
    ++ "\"plugin_job_recovery_cases\":" ++ Conformance.PluginJob.recoveryCasesJson ++ ","
    ++ "\"plugin_job_wake_cases\":" ++ Conformance.PluginJob.wakeCasesJson ++ ","
    ++ "\"tool_timeout_cases\":"
      ++ Conformance.ToolTimeouts.casesJson ++ ","
    ++ "\"scope_collection_rules\":{"
      ++ "\"machineRules\":"
        ++ scopeRulesJson ScopeTemplates.machineRules
      ++ "},"
    ++ "\"scope_node_route_cases\":" ++ nodeRouteCasesJson ++ ","
    ++ "\"scope_template_cases\":" ++ scopeTemplateCasesJson ++ ","
    ++ "\"causal_hop_default_max_request_hop\":"
      ++ Conformance.CausalHopContracts.defaultMaxRequestHopJson ++ ","
    ++ "\"causal_hop_cap_observation_cases\":"
      ++ Conformance.CausalHopContracts.capObservationCasesJson ++ ","
    ++ "\"causal_hop_step_cases\":"
      ++ Conformance.CausalHopContracts.stepCasesJson ++ ","
    ++ "\"causal_hop_chain_cases\":"
      ++ Conformance.CausalHopContracts.chainCasesJson ++ ","
    ++ "\"causal_hop_call_cases\":"
      ++ Conformance.CausalHopContracts.callCasesJson ++ ","
    ++ "\"causal_hop_interrupt_cases\":"
      ++ Conformance.CausalHopContracts.interruptCasesJson ++ ","
    ++ "\"causal_hop_creation_cases\":"
      ++ Conformance.CausalHopContracts.creationCasesJson ++ ","
    ++ "\"causal_hop_creation_signer_cases\":"
      ++ Conformance.CausalHopContracts.creationSignerCasesJson ++ ","
    ++ "\"causal_hop_job_creation_cases\":"
      ++ Conformance.CausalHopContracts.jobCreationCasesJson ++ ","
    ++ "\"causal_hop_deferred_creation_cases\":"
      ++ Conformance.CausalHopContracts.deferredCreationCasesJson ++ ","
    ++ "\"causal_hop_write_cases\":"
      ++ Conformance.CausalHopContracts.writeCasesJson ++ ","
    ++ "\"causal_hop_session_target_cases\":"
      ++ Conformance.CausalHopContracts.sessionTargetCasesJson ++ ","
    ++ "\"causal_hop_session_list_cases\":"
      ++ Conformance.CausalHopContracts.sessionListCasesJson ++ ","
    ++ "\"operator_base_freeze_cases\":"
      ++ Conformance.OperatorBaseFreezeContracts.casesJson ++ ","
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
    ++ "\"file_inventory_order_window_cases\":"
      ++ Conformance.FileInventoryContracts.orderingWindowCasesJson ++ ","
    ++ "\"file_inventory_order_resolution_cases\":"
      ++ Conformance.FileInventoryContracts.orderingResolutionCasesJson ++ ","
    ++ "\"file_inventory_order_comparison_cases\":"
      ++ Conformance.FileInventoryContracts.orderingComparisonCasesJson ++ ","
    ++ "\"file_inventory_order_entry_cases\":"
      ++ Conformance.FileInventoryContracts.orderingEntryCasesJson ++ ","
    ++ "\"file_review_route_cases\":"
      ++ Conformance.FileReviewRouteContracts.routeCasesJson ++ ","
    ++ "\"file_review_subscription_cases\":"
      ++ Conformance.FileReviewRouteContracts.subscriptionCasesJson ++ ","

def snapshotFileCasesJson : String :=
  ""
    ++ "\"workspace_path_alias_cases\":"
      ++ Conformance.WorkspacePathCapabilityContracts.aliasCasesJson ++ ","
    ++ "\"workspace_cases\":"
      ++ workspaceCasesJson ++ ","
    ++ "\"callback_transition_cases\":" ++ callbackTransitionCasesJson ++ ","
    ++ "\"callback_retry_cases\":" ++ callbackRetryCasesJson ++ ","
    ++ "\"callback_recovery_cases\":" ++ callbackRecoveryCasesJson ++ ","
    ++ "\"callback_cases\":"
      ++ callbackCasesJson ++ ","
    ++ "\"r6_background_theorem_witnesses\":"
      ++ jsonArray
        (r6BackgroundTheoremWitnesses.map backgroundTheoremWitnessJson) ++ ","
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
    ++ "\"reasoning_delta_cases\":" ++ jsonArray
      (StreamingResponse.ReasoningDeltas.cases.map reasoningDeltaCaseJson) ++ ","
    ++ "\"auxiliary_output_cases\":" ++ jsonArray
      (CanonicalOutput.Execution.AuxiliaryCases.cases.map auxiliaryOutputCaseJson) ++ ","
    ++ "\"routing_affinity_cases\":" ++ routingAffinityCasesJson ++ ","
    ++ "\"prompt_assembly_service_tier_cases\":" ++
      promptAssemblyServiceTierCasesJson ++ ","
    ++ "\"prompt_assembly_responses_storage_cases\":" ++
      promptAssemblyResponsesStorageCasesJson ++ ","
    ++ "\"prompt_assembly_responses_effort_cases\":" ++
      promptAssemblyResponsesEffortCasesJson ++ ","
    ++ "\"retry_frontier_cases\":" ++ retryFrontierCasesJson ++ ","
    ++ "\"retry_entry_cases\":" ++ retryEntryCasesJson ++ ","
    ++ "\"current_input_cases\":" ++ currentInputCasesJson ++ ","
    ++ "\"wake_input_cases\":" ++ wakeInputCasesJson ++ ","
    ++ "\"prompt_assembly_sanitize_cases\":"
      ++ promptAssemblySanitizeCasesJson ++ ","
    ++ "\"provider_tool_identity_cases\":" ++ providerToolIdentityCasesJson ++ ","

    ++ "\"discovery_guidance_cases\":" ++ discoveryGuidanceCasesJson ++ ","
    ++ "\"prompt_assembly_assistant_order_cases\":"
      ++ promptAssemblyAssistantOrderCasesJson ++ ","
    ++ "\"prompt_assembly_mode_sanitize_cases\":"
      ++ promptAssemblyModeSanitizeCasesJson ++ ","
    ++ "\"prompt_assembly_layer_cases\":"
      ++ promptAssemblyLayerCasesJson ++ ","
    ++ "\"prompt_assembly_repair_cases\":"
      ++ promptAssemblyRepairCasesJson ++ ","
    ++ "\"prompt_assembly_budget_cases\":"
      ++ promptAssemblyBudgetCasesJson ++ ","
    ++ "\"prompt_assembly_turn_budget_cases\":"
      ++ promptAssemblyTurnBudgetCasesJson ++ ","

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
