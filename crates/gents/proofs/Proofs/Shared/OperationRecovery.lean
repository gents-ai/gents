/-! The error-recovery (`NextCall`) contracts for every operation declared in
`gents_protocol::operations`, plus the session state, stop status, and wire
error-code decisions in `operations/session.rs` and `operations/request.rs`.

Each refused operation names the concrete next call a caller (CLI, desktop UI,
or an Agent using self-config tools) should take to correct the failure. Keeping
all recovery tables in one Lean owner guarantees:
- every error variant maps to a well-formed `NextCall` (or an explicit `none`
  only for self-contained install-repair blockers and non-refusal schema
  decisions);
- every `verbHelp`, `preview`, and `verb` names a valid `(resource, verb)` pair,
  while `resourceHelp`, `inspect`, and `list` carry no verb;
- no recovery `verb` loops into a destructive mutation;
- missing-target and authorization refusals direct callers to `list` or
  `inspect` rather than bare help text;
- delegating operation errors (`mcp-service`, `skill`, `tools`) preserve their
  own resource across `DocumentOperationError` and `DocumentDeleteError`;
- `AgentOperationError::recovery` and `edit_recovery` agree on every non-help
  recovery target (`CatalogNotHeld`, `AgentNotHeld`, `SelfDisable`,
  `DefaultAgentDisabled`, `AgentMissing`, and missing `AgentContext`);
- `InferenceOperationError` variants that carry an `InferenceDocument`
  (`Exists`, `Missing`, `IdRequired`) always recover on their own document's
  resource. -/

namespace OperationRecovery

/-- The kind of corrective call a front end renders in its own syntax. -/
inductive NextCallKind where
  | verbHelp
  | resourceHelp
  | inspect
  | list
  | preview
  | verb
  deriving DecidableEq, Repr

/-- The concrete corrective call returned by an operation error's `recovery` or
`next_call` method. -/
structure NextCall where
  kind : NextCallKind
  resource : String
  verb : Option String
  deriving DecidableEq, Repr

def NextCall.verbHelp (resource verb : String) : NextCall :=
  { kind := .verbHelp, resource, verb := some verb }

def NextCall.resourceHelp (resource : String) : NextCall :=
  { kind := .resourceHelp, resource, verb := none }

def NextCall.inspect (resource : String) : NextCall :=
  { kind := .inspect, resource, verb := none }

def NextCall.list (resource : String) : NextCall :=
  { kind := .list, resource, verb := none }

def NextCall.preview (resource verb : String) : NextCall :=
  { kind := .preview, resource, verb := some verb }

def NextCall.runVerb (resource verb : String) : NextCall :=
  { kind := .verb, resource, verb := some verb }

/-- The closed set of protocol resources that operation recoveries may address. -/
def validResource : String → Bool
  | "agent"
  | "backend"
  | "collection"
  | "context"
  | "goal"
  | "mailbox"
  | "mcp-service"
  | "node"
  | "p2p"
  | "p2p-template"
  | "pack"
  | "profile"
  | "request"
  | "schedule"
  | "server"
  | "service"
  | "session"
  | "skill"
  | "task"
  | "tools"
  | "trigger"
  | "workspace-root" => true
  | _ => false

/-- Valid `(resource, verb)` pairs for `verbHelp` recoveries. -/
def validVerbHelpPair : String → String → Bool
  | "agent", "create"
  | "agent", "update"
  | "backend", "create"
  | "backend", "update"
  | "backend", "remove"
  | "backend", "rm"
  | "collection", "update"
  | "context", "delete"
  | "goal", "start"
  | "goal", "set"
  | "goal", "resume-request"
  | "goal", "policy"
  | "mailbox", "reply"
  | "mcp-service", "register"
  | "mcp-service", "delete"
  | "pack", "install"
  | "pack", "update"
  | "pack", "remove"
  | "pack", "wire"
  | "profile", "create"
  | "profile", "update"
  | "profile", "remove"
  | "profile", "rm"
  | "request", "submit"
  | "server", "run"
  | "service", "update-status"
  | "session", "fork"
  | "skill", "import"
  | "skill", "delete"
  | "task", "run"
  | "tools", "update"
  | "tools", "delete"
  | "workspace-root", "publish"
  | "workspace-root", "grant-reader"
  | "workspace-root", "revoke-reader"
  | "workspace-root", "remove"
  | "workspace-root", "show" => true
  | _, _ => false

/-- Valid `(resource, verb)` pairs for `preview` recoveries. -/
def validPreviewPair : String → String → Bool
  | "pack", "install"
  | "pack", "update"
  | "pack", "remove"
  | "pack", "wire" => true
  | _, _ => false

/-- Valid `(resource, verb)` pairs for direct `verb` recoveries (read or start
calls only; never destructive mutations). -/
def validDirectVerbPair : String → String → Bool
  | "backend", "accounts"
  | "goal", "start"
  | "node", "status" => true
  | _, _ => false

/-- A well-formed `NextCall` names a known resource and carries a verb iff its
kind expects one, from the allowed pairs for that kind. -/
def NextCall.wellFormed (call : NextCall) : Bool :=
  validResource call.resource &&
  match call.kind, call.verb with
  | .verbHelp, some v => validVerbHelpPair call.resource v
  | .preview, some v => validPreviewPair call.resource v
  | .verb, some v => validDirectVerbPair call.resource v
  | .resourceHelp, none => true
  | .inspect, none => true
  | .list, none => true
  | _, _ => false

/-! ## Agent operations (`operations/agent.rs`) -/

inductive AgentCreateOperation where
  | create
  | clone
  deriving DecidableEq, Repr

/-- `AGENT_CREATE.help` and `AGENT_CLONE.help`. -/
def AgentCreateOperation.help : AgentCreateOperation → NextCall
  | .create => NextCall.verbHelp "agent" "create"
  | .clone => NextCall.resourceHelp "agent"

/-- Every `AgentOperationError` variant; `MissingDocument` splits on whether the
missing collection is `AgentContext`, which lists `context` rather than falling
back to operation help. -/
inductive AgentError where
  | previewRequiresOperation
  | editRejectsCloneFrom
  | editRejectsToolsFields
  | disableAcceptsOnlyTarget
  | cloneRequiresFrom
  | unknownOperation
  | identityBindingRequired
  | identityBindingMissing
  | identityBindingMismatch
  | newAgentDisabled
  | agentDidRequired
  | agentExists
  | displayNameRequired
  | profileNotPublished
  | systemPromptRequired
  | cloneSourceNotEnabled
  | agentMissing
  | catalogNotHeld
  | agentNotHeld
  | unsupportedEditField
  | displayNameEmpty
  | systemPromptBlank
  | contextIdBlank
  | defaultAgentDisabled
  | selfDisable
  | missingAgentContext
  | missingOtherDocument
  | generatedContextIdExists
  | generatedToolsIdExists
  | clonePresetRefused
  | cloneContextIdRefused
  | createContextIdConflicts
  | unknownPreset
  | cloneSourceGrantNotHeld
  | objectRequired
  | contextPatchMismatch
  | agentHasNoContext
  | systemPromptContextShared
  | admissionActorMismatch
  | candidateIdentityMissing
  | candidateDoesNotResolve
  | passthrough
  deriving DecidableEq, Repr

/-- Recovery shared across agent create, clone, and edit when the error names a
concrete discovery call rather than operation help. -/
def agentTargetRecovery : AgentError → Option NextCall
  | .catalogNotHeld | .agentNotHeld | .selfDisable => some (NextCall.inspect "agent")
  | .defaultAgentDisabled | .agentMissing => some (NextCall.list "agent")
  | .missingAgentContext => some (NextCall.list "context")
  | .profileNotPublished => some (NextCall.list "profile")
  | _ => none

def agentRecovery (error : AgentError) (operation : AgentCreateOperation) : NextCall :=
  (agentTargetRecovery error).getD operation.help

def agentEditRecovery (error : AgentError) : NextCall :=
  (agentTargetRecovery error).getD (NextCall.verbHelp "agent" "update")

/-! ## Generic document operations (`operations/document.rs`) -/

/-- The document resources handled through `DocumentOperationError` and
`DocumentDeleteError`. -/
inductive DocumentResource where
  | context
  | mcpService
  | skill
  | tools
  deriving DecidableEq, Repr

def DocumentResource.wireName : DocumentResource → String
  | .context => "context"
  | .mcpService => "mcp-service"
  | .skill => "skill"
  | .tools => "tools"

inductive DocumentError where
  | idRequired
  | «exists»
  | missing
  | ambiguousOwner
  | invalid
  | passthrough
  deriving DecidableEq, Repr

def documentRecovery (error : DocumentError) (resource : DocumentResource) : NextCall :=
  let res := resource.wireName
  match error with
  | .«exists» => NextCall.inspect res
  | .missing | .ambiguousOwner => NextCall.list res
  | .idRequired | .invalid | .passthrough => NextCall.resourceHelp res

inductive DocumentDeleteError where
  | noTargets
  | duplicateTarget
  | targetMissing
  | digestRequired
  | digestStale
  | stillReferenced
  | notRemoved
  | passthrough
  deriving DecidableEq, Repr

def documentDeleteRecovery (error : DocumentDeleteError) (resource : DocumentResource) : NextCall :=
  let res := resource.wireName
  match error with
  | .targetMissing => NextCall.list res
  | .stillReferenced => NextCall.inspect res
  | .noTargets | .duplicateTarget | .digestRequired | .digestStale
  | .notRemoved | .passthrough => NextCall.verbHelp res "delete"

/-! ## Goal operations (`operations/goal.rs`) -/

inductive GoalError where
  | denied
  | sessionMissing
  | noGoal
  | busy
  | liveGoal
  | stale
  | conflict
  | invalidObjective
  | invalidBudget
  | invalidCeiling
  | confirmBlocked
  | keepTrying
  | nothingToChange
  | runtimeOnly
  | personOnly
  | fieldsNeedKeep
  | notEditable
  | resumeRequired
  | obligationsUnmet
  | firstRequestInvalid
  | firstRequestMismatch
  | reasonNotAccepted
  | reasonRequired
  | illegalTransition
  | passthrough
  deriving DecidableEq, Repr

def goalRecovery (error : GoalError) : NextCall :=
  match error with
  | .denied | .sessionMissing => NextCall.list "session"
  | .noGoal => NextCall.runVerb "goal" "start"
  | .firstRequestInvalid | .firstRequestMismatch => NextCall.verbHelp "goal" "start"
  | .busy | .liveGoal | .stale | .conflict | .notEditable
  | .illegalTransition | .confirmBlocked | .keepTrying => NextCall.inspect "goal"
  | .resumeRequired => NextCall.verbHelp "goal" "resume-request"
  | .invalidCeiling => NextCall.verbHelp "goal" "policy"
  | .invalidObjective | .invalidBudget | .nothingToChange | .runtimeOnly
  | .personOnly | .fieldsNeedKeep | .obligationsUnmet | .reasonNotAccepted
  | .reasonRequired | .passthrough => NextCall.verbHelp "goal" "set"

/-! ## Inference operations (`operations/inference.rs`) -/

inductive InferenceDocument where
  | backend
  | profile
  deriving DecidableEq, Repr

def InferenceDocument.wireName : InferenceDocument → String
  | .backend => "backend"
  | .profile => "profile"

inductive InferenceVerb where
  | create
  | update
  | remove
  | rm
  deriving DecidableEq, Repr

def InferenceVerb.wireName : InferenceVerb → String
  | .create => "create"
  | .update => "update"
  | .remove => "remove"
  | .rm => "rm"

inductive InferenceError where
  | rawCredential
  | oauthAccount
  | oauthPool
  | localServerOnly
  | startsEnabled
  | otherAccount
  | otherPool
  | accountOperation
  | existsBackend
  | existsProfile
  | missingBackend
  | missingProfile
  | idRequiredBackend
  | idRequiredProfile
  | invalid
  | passthrough
  deriving DecidableEq, Repr

def inferenceRecovery (error : InferenceError) (document : InferenceDocument)
    (verb : InferenceVerb) : NextCall :=
  match error with
  | .rawCredential | .oauthAccount | .oauthPool | .localServerOnly | .startsEnabled =>
      NextCall.resourceHelp "backend"
  | .otherAccount | .otherPool | .accountOperation => NextCall.runVerb "backend" "accounts"
  | .missingBackend => NextCall.list "backend"
  | .missingProfile => NextCall.list "profile"
  | .existsBackend => NextCall.verbHelp "backend" "update"
  | .existsProfile => NextCall.verbHelp "profile" "update"
  | .idRequiredBackend => NextCall.verbHelp "backend" verb.wireName
  | .idRequiredProfile => NextCall.verbHelp "profile" verb.wireName
  | .invalid | .passthrough => NextCall.verbHelp document.wireName verb.wireName

/-! ## Install-repair blockers (`operations/install_repair.rs`) -/

inductive InstallRepairBlocker where
  | portInUse
  | handStartedNode
  | keychainUnavailable
  | storeRefused
  | cannotSetAside
  | keepsExiting
  | noAnswer
  | noServiceManager
  | homeUnreadable
  | homeBusy
  | oldNodeStillRunning
  | serviceUpdateFailed
  | backgroundApprovalPending
  | newerNode
  | serviceServesAnotherHome
  deriving DecidableEq, Repr

/-- Blockers whose Display text already states the exact user action return
`none`; all others name a concrete follow-up call. -/
def installRepairBlockerNextCall (blocker : InstallRepairBlocker) : Option NextCall :=
  match blocker with
  | .storeRefused | .serviceServesAnotherHome | .newerNode | .backgroundApprovalPending => none
  | .noServiceManager => some (NextCall.verbHelp "server" "run")
  | .serviceUpdateFailed => some (NextCall.verbHelp "service" "update-status")
  | .portInUse | .handStartedNode | .keychainUnavailable | .cannotSetAside
  | .keepsExiting | .noAnswer | .homeUnreadable | .homeBusy | .oldNodeStillRunning =>
      some (NextCall.runVerb "node" "status")

/-! ## Mailbox operations (`operations/mailbox.rs`) -/

inductive MailboxError where
  | itemMissing
  | itemAmbiguous
  | unknownStatus
  | unknownAction
  | notOpen
  | notReplyable
  | notAQuestion
  | routeDiffers
  | questionChanged
  | needsAnswer
  | questionPayloadMissing
  | actorRequired
  | dismissalLost
  | question
  | questionReply
  | passthrough
  deriving DecidableEq, Repr

def mailboxRecovery (error : MailboxError) : NextCall :=
  match error with
  | .itemMissing | .itemAmbiguous | .notOpen | .unknownStatus
  | .unknownAction | .dismissalLost => NextCall.list "mailbox"
  | .notReplyable | .routeDiffers | .questionPayloadMissing
  | .questionChanged | .question | .questionReply => NextCall.inspect "mailbox"
  | .notAQuestion | .needsAnswer => NextCall.verbHelp "mailbox" "reply"
  | .actorRequired | .passthrough => NextCall.resourceHelp "mailbox"

/-! ## MCP service operations (`operations/mcp_service.rs`) -/

inductive McpServiceError where
  | endpoint
  | documentIdRequired
  | documentExists
  | documentMissing
  | documentAmbiguousOwner
  | documentInvalid
  | documentPassthrough
  | deleteNoTargets
  | deleteDuplicateTarget
  | deleteTargetMissing
  | deleteDigestRequired
  | deleteDigestStale
  | deleteStillReferenced
  | deleteNotRemoved
  | deletePassthrough
  deriving DecidableEq, Repr

def mcpServiceRecovery (error : McpServiceError) : NextCall :=
  match error with
  | .endpoint => NextCall.verbHelp "mcp-service" "register"
  | .documentIdRequired => documentRecovery .idRequired .mcpService
  | .documentExists => documentRecovery .«exists» .mcpService
  | .documentMissing => documentRecovery .missing .mcpService
  | .documentAmbiguousOwner => documentRecovery .ambiguousOwner .mcpService
  | .documentInvalid => documentRecovery .invalid .mcpService
  | .documentPassthrough => documentRecovery .passthrough .mcpService
  | .deleteNoTargets => documentDeleteRecovery .noTargets .mcpService
  | .deleteDuplicateTarget => documentDeleteRecovery .duplicateTarget .mcpService
  | .deleteTargetMissing => documentDeleteRecovery .targetMissing .mcpService
  | .deleteDigestRequired => documentDeleteRecovery .digestRequired .mcpService
  | .deleteDigestStale => documentDeleteRecovery .digestStale .mcpService
  | .deleteStillReferenced => documentDeleteRecovery .stillReferenced .mcpService
  | .deleteNotRemoved => documentDeleteRecovery .notRemoved .mcpService
  | .deletePassthrough => documentDeleteRecovery .passthrough .mcpService

/-! ## P2P peer-registry operations (`operations/p2p.rs`) -/

inductive RegistryError where
  | peerIdUnknown
  | unknownTemplate
  | passthrough
  deriving DecidableEq, Repr

def registryRecovery (error : RegistryError) : NextCall :=
  match error with
  | .peerIdUnknown | .passthrough => NextCall.inspect "p2p"
  | .unknownTemplate => NextCall.list "p2p-template"

/-! ## Pack operations (`operations/pack.rs`) -/

inductive PackOperation where
  | install
  | update
  | remove
  | wire
  deriving DecidableEq, Repr

def PackOperation.wireName : PackOperation → String
  | .install => "install"
  | .update => "update"
  | .remove => "remove"
  | .wire => "wire"

inductive PackError where
  | packageBlank
  | previewRequired
  | digestChanged
  | editedUndecided
  | editedWord
  | variableMalformed
  | variableName
  | variableInference
  | variableTooLarge
  | variableDuplicate
  | tooManyVariables
  | tooManySlots
  | slotMalformed
  | slotDuplicate
  | passthrough
  deriving DecidableEq, Repr

def packRecovery (error : PackError) (operation : PackOperation) : NextCall :=
  match error with
  | .previewRequired | .digestChanged | .editedUndecided =>
      NextCall.preview "pack" operation.wireName
  | .packageBlank => NextCall.list "pack"
  | .editedWord | .variableMalformed | .variableName | .variableInference
  | .variableTooLarge | .variableDuplicate | .tooManyVariables | .tooManySlots
  | .slotMalformed | .slotDuplicate | .passthrough =>
      NextCall.verbHelp "pack" operation.wireName

/-! ## Agent permission operations (`operations/permission.rs`) -/

inductive AgentPermissionError where
  | invalidAgentDid
  | agentUnavailable
  | nodeUnavailable
  | passthrough
  deriving DecidableEq, Repr

def agentPermissionRecovery (error : AgentPermissionError) : NextCall :=
  match error with
  | .invalidAgentDid | .agentUnavailable => NextCall.list "agent"
  | .nodeUnavailable | .passthrough => NextCall.inspect "node"

/-! ## Request operations (`operations/request.rs`) -/

inductive RequestError where
  | fieldRequired
  | contentRequired
  | identityRequired
  | queueEntryOwned
  | conflictingActiveRequests
  | nodeUnavailable
  | noDefaultAgent
  | agentUnavailable
  | sessionAgentMismatch
  | mailboxCause
  | notFound
  | ambiguous
  | reissueParentState
  | reissueNotInteractive
  | retryBudgetExhausted
  | reissueDeadlineClosed
  | reissueNotLatest
  | waitTimedOut
  | outputUnavailable
  | successorInvalid
  | receiptMismatch
  | interruptRefused
  | unconfirmed
  | identityConflict
  | passthrough
  deriving DecidableEq, Repr

def requestErrorCode (error : RequestError) : String :=
  match error with
  | .fieldRequired => "field_required"
  | .contentRequired => "content_required"
  | .identityRequired => "identity_required"
  | .queueEntryOwned => "queue_entry_owned"
  | .conflictingActiveRequests => "conflicting_active_requests"
  | .nodeUnavailable => "node_unavailable"
  | .noDefaultAgent => "no_default_agent"
  | .agentUnavailable => "agent_unavailable"
  | .sessionAgentMismatch => "session_agent_mismatch"
  | .mailboxCause => "mailbox_cause"
  | .notFound => "request_not_found"
  | .ambiguous => "request_ambiguous"
  | .reissueParentState => "reissue_parent_state"
  | .reissueNotInteractive => "reissue_not_interactive"
  | .retryBudgetExhausted => "retry_budget_exhausted"
  | .reissueDeadlineClosed => "reissue_deadline_closed"
  | .reissueNotLatest => "reissue_not_latest"
  | .waitTimedOut => "wait_timed_out"
  | .outputUnavailable => "output_unavailable"
  | .successorInvalid => "successor_invalid"
  | .receiptMismatch => "receipt_mismatch"
  | .interruptRefused => "interrupt_refused"
  | .unconfirmed => "submission_unconfirmed"
  | .identityConflict => "request_identity_conflict"
  | .passthrough => "failed"

def requestNextCall (error : RequestError) : NextCall :=
  match error with
  | .fieldRequired | .contentRequired | .queueEntryOwned | .retryBudgetExhausted
  | .reissueDeadlineClosed | .unconfirmed | .identityConflict | .receiptMismatch
  | .passthrough => NextCall.verbHelp "request" "submit"
  | .identityRequired | .nodeUnavailable => NextCall.inspect "node"
  | .noDefaultAgent | .agentUnavailable | .sessionAgentMismatch => NextCall.list "agent"
  | .mailboxCause => NextCall.list "mailbox"
  | .conflictingActiveRequests | .reissueNotLatest | .notFound | .ambiguous =>
      NextCall.list "session"
  | .reissueParentState | .reissueNotInteractive | .waitTimedOut
  | .outputUnavailable | .successorInvalid | .interruptRefused =>
      NextCall.inspect "request"

/-! ## Schema install decisions (`operations/schema.rs`) -/

inductive SchemaInstallDecision where
  | publish
  | satisfied
  | awaitsFields
  | fieldsPending
  | mismatch
  deriving DecidableEq, Repr

def schemaInstallRecovery (decision : SchemaInstallDecision) : Option NextCall :=
  match decision with
  | .fieldsPending => some (NextCall.verbHelp "collection" "update")
  | .mismatch => some (NextCall.inspect "collection")
  | .publish | .satisfied | .awaitsFields => none

/-! ## Session operations (`operations/session.rs`) -/

inductive SessionState where
  | «open»
  | closed
  | archived
  deriving DecidableEq, Repr

/-- `SessionState::of`: `archived_at` dominates `closed_at`. -/
def sessionState (hasClosedAt hasArchivedAt : Bool) : SessionState :=
  if hasArchivedAt then .archived
  else if hasClosedAt then .closed
  else .«open»

inductive SessionStopStatus where
  | stopping
  | stopRequested
  deriving DecidableEq, Repr

/-- `SessionStopStatus::of`: interrupting an active request reports `stopping`;
otherwise `stop_requested`. -/
def sessionStopStatus (interruptedARequest : Bool) : SessionStopStatus :=
  if interruptedARequest then .stopping else .stopRequested

inductive SessionError where
  | fieldRequired
  | identityRequired
  | notFound
  | ambiguous
  | invalidDocument
  | requesterMissing
  | revisionOutOfRange
  | requesterMismatch
  | forkSourceBusy
  | forkTurnOutOfRange
  | forkMessageNotEditable
  | forkClientMismatch
  | forkChildNotCanonical
  | crossAgentForkUnavailable
  | agentUnavailable
  | forkRelationshipsIncomplete
  | forkRecoveryIntentRequired
  | remoteNodeUnsupported
  | resumeBlocked
  | processOwnershipUnverifiable
  | passthrough
  deriving DecidableEq, Repr

def sessionErrorCode (error : SessionError) : String :=
  match error with
  | .fieldRequired => "field_required"
  | .identityRequired => "identity_required"
  | .notFound => "session_not_found"
  | .ambiguous => "session_ambiguous"
  | .invalidDocument => "session_invalid_document"
  | .requesterMissing => "session_requester_missing"
  | .revisionOutOfRange => "archive_revision_out_of_range"
  | .requesterMismatch => "session_requester_mismatch"
  | .forkSourceBusy => "fork_source_busy"
  | .forkTurnOutOfRange => "fork_turn_out_of_range"
  | .forkMessageNotEditable => "fork_message_not_editable"
  | .forkClientMismatch => "fork_client_mismatch"
  | .forkChildNotCanonical => "fork_child_not_canonical"
  | .crossAgentForkUnavailable => "cross_agent_fork_unavailable"
  | .agentUnavailable => "agent_unavailable"
  | .forkRelationshipsIncomplete => "fork_relationships_incomplete"
  | .forkRecoveryIntentRequired => "fork_recovery_intent_required"
  | .remoteNodeUnsupported => "remote_node_unsupported"
  | .resumeBlocked => "resume_blocked"
  | .processOwnershipUnverifiable => "process_ownership_unverifiable"
  | .passthrough => "failed"

def sessionNextCall (error : SessionError) : NextCall :=
  match error with
  | .fieldRequired | .revisionOutOfRange | .forkTurnOutOfRange
  | .forkMessageNotEditable | .forkChildNotCanonical | .crossAgentForkUnavailable
  | .passthrough => NextCall.verbHelp "session" "fork"
  | .identityRequired | .forkClientMismatch | .remoteNodeUnsupported =>
      NextCall.inspect "node"
  | .notFound | .ambiguous | .requesterMismatch => NextCall.list "session"
  | .invalidDocument | .requesterMissing | .forkRelationshipsIncomplete
  | .forkRecoveryIntentRequired => NextCall.inspect "session"
  | .forkSourceBusy | .resumeBlocked | .processOwnershipUnverifiable =>
      NextCall.inspect "request"
  | .agentUnavailable => NextCall.list "agent"

/-! ## Skill operations (`operations/skill.rs`) -/

inductive SkillError where
  | importExists
  | outsideToolRoot
  | fileReadNotGranted
  | toolRootMissing
  | source
  | documentIdRequired
  | documentExists
  | documentMissing
  | documentAmbiguousOwner
  | documentInvalid
  | documentPassthrough
  | deleteNoTargets
  | deleteDuplicateTarget
  | deleteTargetMissing
  | deleteDigestRequired
  | deleteDigestStale
  | deleteStillReferenced
  | deleteNotRemoved
  | deletePassthrough
  deriving DecidableEq, Repr

def skillRecovery (error : SkillError) : NextCall :=
  match error with
  | .importExists | .source => NextCall.verbHelp "skill" "import"
  | .outsideToolRoot | .fileReadNotGranted | .toolRootMissing => NextCall.inspect "tools"
  | .documentIdRequired => documentRecovery .idRequired .skill
  | .documentExists => documentRecovery .«exists» .skill
  | .documentMissing => documentRecovery .missing .skill
  | .documentAmbiguousOwner => documentRecovery .ambiguousOwner .skill
  | .documentInvalid => documentRecovery .invalid .skill
  | .documentPassthrough => documentRecovery .passthrough .skill
  | .deleteNoTargets => documentDeleteRecovery .noTargets .skill
  | .deleteDuplicateTarget => documentDeleteRecovery .duplicateTarget .skill
  | .deleteTargetMissing => documentDeleteRecovery .targetMissing .skill
  | .deleteDigestRequired => documentDeleteRecovery .digestRequired .skill
  | .deleteDigestStale => documentDeleteRecovery .digestStale .skill
  | .deleteStillReferenced => documentDeleteRecovery .stillReferenced .skill
  | .deleteNotRemoved => documentDeleteRecovery .notRemoved .skill
  | .deletePassthrough => documentDeleteRecovery .passthrough .skill

/-! ## Task operations (`operations/task.rs`) -/

inductive TaskError where
  | taskMissing
  | taskDisabled
  | agentMissing
  | agentDisabled
  | commandTask
  | argsNotObject
  | stableKeyRequired
  | sessionLabelAndContinue
  | scheduleMissing
  | scheduleAmbiguous
  | noEnabledTrigger
  | manyEnabledTriggers
  | passthrough
  deriving DecidableEq, Repr

/-- `agentMissing` and `agentDisabled` both inspect `agent` so the desktop
client's recovery renderer distinguishes them from trigger/schedule listings. -/
def taskRecovery (error : TaskError) : NextCall :=
  match error with
  | .taskMissing => NextCall.list "task"
  | .taskDisabled => NextCall.inspect "task"
  | .commandTask | .noEnabledTrigger | .manyEnabledTriggers => NextCall.list "trigger"
  | .agentMissing | .agentDisabled => NextCall.inspect "agent"
  | .argsNotObject | .stableKeyRequired | .sessionLabelAndContinue | .passthrough =>
      NextCall.verbHelp "task" "run"
  | .scheduleMissing | .scheduleAmbiguous => NextCall.list "schedule"

/-! ## Tools operations (`operations/tools.rs`) -/

inductive ToolsError where
  | sharedUnacknowledged
  | sharedWithoutCatalog
  | grantNotHeld
  | losesOwnControl
  | dropsSettings
  | pluginNotInstalled
  | documentIdRequired
  | documentExists
  | documentMissing
  | documentAmbiguousOwner
  | documentInvalid
  | documentPassthrough
  | deleteNoTargets
  | deleteDuplicateTarget
  | deleteTargetMissing
  | deleteDigestRequired
  | deleteDigestStale
  | deleteStillReferenced
  | deleteNotRemoved
  | deletePassthrough
  deriving DecidableEq, Repr

def toolsRecovery (error : ToolsError) : NextCall :=
  match error with
  | .sharedUnacknowledged | .dropsSettings => NextCall.verbHelp "tools" "update"
  | .sharedWithoutCatalog => NextCall.inspect "agent"
  | .grantNotHeld | .losesOwnControl => NextCall.inspect "tools"
  | .pluginNotInstalled => NextCall.list "pack"
  | .documentIdRequired => documentRecovery .idRequired .tools
  | .documentExists => documentRecovery .«exists» .tools
  | .documentMissing => documentRecovery .missing .tools
  | .documentAmbiguousOwner => documentRecovery .ambiguousOwner .tools
  | .documentInvalid => documentRecovery .invalid .tools
  | .documentPassthrough => documentRecovery .passthrough .tools
  | .deleteNoTargets => documentDeleteRecovery .noTargets .tools
  | .deleteDuplicateTarget => documentDeleteRecovery .duplicateTarget .tools
  | .deleteTargetMissing => documentDeleteRecovery .targetMissing .tools
  | .deleteDigestRequired => documentDeleteRecovery .digestRequired .tools
  | .deleteDigestStale => documentDeleteRecovery .digestStale .tools
  | .deleteStillReferenced => documentDeleteRecovery .stillReferenced .tools
  | .deleteNotRemoved => documentDeleteRecovery .notRemoved .tools
  | .deletePassthrough => documentDeleteRecovery .passthrough .tools

/-! ## Workspace-root operations (`operations/workspace_root.rs`) -/

inductive WorkspaceRootVerb where
  | publish
  | grantReader
  | revokeReader
  | remove
  | show
  deriving DecidableEq, Repr

def WorkspaceRootVerb.wireName : WorkspaceRootVerb → String
  | .publish => "publish"
  | .grantReader => "grant-reader"
  | .revokeReader => "revoke-reader"
  | .remove => "remove"
  | .show => "show"

inductive WorkspaceRootError where
  | pathNotAbsolute
  | rootMissing
  | foreignRoot
  | nodeUnavailable
  | agentUnavailable
  | nodeActor
  | passthrough
  deriving DecidableEq, Repr

def workspaceRootRecovery (error : WorkspaceRootError) (verb : WorkspaceRootVerb) : NextCall :=
  match error with
  | .pathNotAbsolute | .passthrough => NextCall.verbHelp "workspace-root" verb.wireName
  | .rootMissing | .foreignRoot => NextCall.list "workspace-root"
  | .nodeUnavailable => NextCall.inspect "node"
  | .agentUnavailable | .nodeActor => NextCall.list "agent"

/-! ## Theorems -/

/-- Every `agentRecovery` output is a well-formed `NextCall`. -/
theorem agent_recovery_well_formed (e : AgentError) (op : AgentCreateOperation) :
    (agentRecovery e op).wellFormed = true := by
  cases e <;> cases op <;> decide

/-- Every `agentEditRecovery` output is a well-formed `NextCall`. -/
theorem agent_edit_recovery_well_formed (e : AgentError) :
    (agentEditRecovery e).wellFormed = true := by
  cases e <;> decide

/-- `agentRecovery` and `agentEditRecovery` agree whenever the error names a
concrete discovery target (inspect or list). -/
theorem agent_recovery_and_edit_recovery_agree_on_targets (e : AgentError)
    (op : AgentCreateOperation) (target : NextCall)
    (h : agentTargetRecovery e = some target) :
    agentRecovery e op = target ∧ agentEditRecovery e = target := by
  simp [agentRecovery, agentEditRecovery, h]

/-- Every `documentRecovery` output is well-formed and addresses the caller's resource. -/
theorem document_recovery_well_formed_and_preserves_resource
    (e : DocumentError) (r : DocumentResource) :
    (documentRecovery e r).wellFormed = true ∧ (documentRecovery e r).resource = r.wireName := by
  cases e <;> cases r <;> decide

/-- Every `documentDeleteRecovery` output is well-formed and addresses the caller's resource. -/
theorem document_delete_recovery_well_formed_and_preserves_resource
    (e : DocumentDeleteError) (r : DocumentResource) :
    (documentDeleteRecovery e r).wellFormed = true ∧
      (documentDeleteRecovery e r).resource = r.wireName := by
  cases e <;> cases r <;> decide

/-- Every `goalRecovery` output is a well-formed `NextCall`. -/
theorem goal_recovery_well_formed (e : GoalError) :
    (goalRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `inferenceRecovery` output is a well-formed `NextCall`. -/
theorem inference_recovery_well_formed (e : InferenceError) (d : InferenceDocument)
    (v : InferenceVerb) :
    (inferenceRecovery e d v).wellFormed = true := by
  cases e <;> cases d <;> cases v <;> decide

/-- `InferenceError` variants that carry their own document (`Exists`, `Missing`,
`IdRequired`) always target that document's resource regardless of the fallback
document argument. -/
theorem inference_document_errors_target_own_document (d : InferenceDocument) (v : InferenceVerb) :
    (inferenceRecovery .missingBackend d v).resource = "backend" ∧
    (inferenceRecovery .missingProfile d v).resource = "profile" ∧
    (inferenceRecovery .existsBackend d v).resource = "backend" ∧
    (inferenceRecovery .existsProfile d v).resource = "profile" ∧
    (inferenceRecovery .idRequiredBackend d v).resource = "backend" ∧
    (inferenceRecovery .idRequiredProfile d v).resource = "profile" := by
  cases d <;> cases v <;> decide

/-- Every `installRepairBlockerNextCall` output is `none` iff the blocker's
message is self-contained, and well-formed whenever `some`. -/
theorem install_repair_blocker_recovery_contract (b : InstallRepairBlocker) :
    (installRepairBlockerNextCall b = none ↔
      b = .storeRefused ∨ b = .serviceServesAnotherHome ∨
      b = .newerNode ∨ b = .backgroundApprovalPending) ∧
    (∀ call, installRepairBlockerNextCall b = some call → call.wellFormed = true) := by
  constructor
  · cases b <;> decide
  · intro call h
    cases b <;> simp [installRepairBlockerNextCall] at h <;> subst h <;> decide

/-- Every `mailboxRecovery` output is a well-formed `NextCall`. -/
theorem mailbox_recovery_well_formed (e : MailboxError) :
    (mailboxRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `mcpServiceRecovery` output is a well-formed `NextCall`. -/
theorem mcp_service_recovery_well_formed (e : McpServiceError) :
    (mcpServiceRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `registryRecovery` output is a well-formed `NextCall`. -/
theorem registry_recovery_well_formed (e : RegistryError) :
    (registryRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `packRecovery` output is well-formed, and previewable errors preserve
the exact pack operation verb. -/
theorem pack_recovery_well_formed_and_preserves_preview_verb
    (e : PackError) (op : PackOperation) :
    (packRecovery e op).wellFormed = true ∧
    ((e = .previewRequired ∨ e = .digestChanged ∨ e = .editedUndecided) →
      packRecovery e op = NextCall.preview "pack" op.wireName) := by
  cases e <;> cases op <;> decide

/-- Every `agentPermissionRecovery` output is a well-formed `NextCall`. -/
theorem agent_permission_recovery_well_formed (e : AgentPermissionError) :
    (agentPermissionRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `requestNextCall` output is a well-formed `NextCall`. -/
theorem request_next_call_well_formed (e : RequestError) :
    (requestNextCall e).wellFormed = true := by
  cases e <;> decide

/-- `requestErrorCode` is non-empty and injective across all 25 variants. -/
theorem request_error_codes_nonempty_and_unique (a b : RequestError) :
    (requestErrorCode a ≠ "") ∧ (requestErrorCode a = requestErrorCode b → a = b) := by
  cases a <;> cases b <;> decide

/-- `schemaInstallRecovery` returns `some` well-formed call iff the decision is
a refusal (`fieldsPending` or `mismatch`). -/
theorem schema_install_recovery_contract (d : SchemaInstallDecision) :
    (schemaInstallRecovery d = none ↔
      d = .publish ∨ d = .satisfied ∨ d = .awaitsFields) ∧
    (∀ call, schemaInstallRecovery d = some call → call.wellFormed = true) := by
  constructor
  · cases d <;> decide
  · intro call h
    cases d <;> simp [schemaInstallRecovery] at h <;> subst h <;> decide

/-- Every `sessionNextCall` output is a well-formed `NextCall`. -/
theorem session_next_call_well_formed (e : SessionError) :
    (sessionNextCall e).wellFormed = true := by
  cases e <;> decide

/-- `sessionErrorCode` is non-empty and injective across all 21 variants. -/
theorem session_error_codes_nonempty_and_unique (a b : SessionError) :
    (sessionErrorCode a ≠ "") ∧ (sessionErrorCode a = sessionErrorCode b → a = b) := by
  cases a <;> cases b <;> decide

/-- `archived` dominates `closed` in `sessionState`. -/
theorem session_state_archived_dominates_closed (hasClosedAt : Bool) :
    sessionState hasClosedAt true = .archived := by
  cases hasClosedAt <;> rfl

/-- Every `skillRecovery` output is a well-formed `NextCall`. -/
theorem skill_recovery_well_formed (e : SkillError) :
    (skillRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `taskRecovery` output is a well-formed `NextCall`. -/
theorem task_recovery_well_formed (e : TaskError) :
    (taskRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `toolsRecovery` output is a well-formed `NextCall`. -/
theorem tools_recovery_well_formed (e : ToolsError) :
    (toolsRecovery e).wellFormed = true := by
  cases e <;> decide

/-- Every `workspaceRootRecovery` output is a well-formed `NextCall`. -/
theorem workspace_root_recovery_well_formed (e : WorkspaceRootError) (v : WorkspaceRootVerb) :
    (workspaceRootRecovery e v).wellFormed = true := by
  cases e <;> cases v <;> decide

/-- No `NextCall.verb` recovery across the entire protocol triggers a destructive
verb (`delete`, `remove`, `rm`, `disable`, `uninstall`). -/
theorem no_well_formed_direct_verb_is_destructive (call : NextCall)
    (hw : call.wellFormed = true) (hv : call.kind = .verb) :
    call.verb ≠ some "delete" ∧ call.verb ≠ some "remove" ∧
    call.verb ≠ some "rm" ∧ call.verb ≠ some "disable" ∧
    call.verb ≠ some "uninstall" := by
  cases call with
  | mk kind resource verb =>
    subst hv
    cases verb with
    | none => simp [NextCall.wellFormed] at hw
    | some v =>
      simp only [NextCall.wellFormed, Bool.and_eq_true] at hw
      have hdv := hw.2
      unfold validDirectVerbPair at hdv
      split at hdv <;> simp_all

/-- Missing-target errors across operations direct callers to `list` or
`inspect` rather than bare help text. -/
theorem missing_target_errors_discover_instead_of_help
    (op : AgentCreateOperation) (r : DocumentResource) (d : InferenceDocument)
    (v : InferenceVerb) (wv : WorkspaceRootVerb) :
    let isDiscovery (c : NextCall) := c.kind = .list ∨ c.kind = .inspect
    isDiscovery (agentRecovery .agentMissing op) ∧
    isDiscovery (agentEditRecovery .agentMissing) ∧
    isDiscovery (agentRecovery .missingAgentContext op) ∧
    isDiscovery (agentEditRecovery .missingAgentContext) ∧
    isDiscovery (agentRecovery .profileNotPublished op) ∧
    isDiscovery (agentEditRecovery .profileNotPublished) ∧
    isDiscovery (documentRecovery .missing r) ∧
    isDiscovery (documentDeleteRecovery .targetMissing r) ∧
    isDiscovery (goalRecovery .sessionMissing) ∧
    isDiscovery (inferenceRecovery .missingBackend d v) ∧
    isDiscovery (inferenceRecovery .missingProfile d v) ∧
    isDiscovery (mailboxRecovery .itemMissing) ∧
    isDiscovery (agentPermissionRecovery .agentUnavailable) ∧
    isDiscovery (agentPermissionRecovery .nodeUnavailable) ∧
    isDiscovery (requestNextCall .notFound) ∧
    isDiscovery (requestNextCall .nodeUnavailable) ∧
    isDiscovery (requestNextCall .agentUnavailable) ∧
    isDiscovery (sessionNextCall .notFound) ∧
    isDiscovery (sessionNextCall .agentUnavailable) ∧
    isDiscovery (taskRecovery .taskMissing) ∧
    isDiscovery (taskRecovery .agentMissing) ∧
    isDiscovery (taskRecovery .scheduleMissing) ∧
    isDiscovery (workspaceRootRecovery .rootMissing wv) ∧
    isDiscovery (workspaceRootRecovery .nodeUnavailable wv) ∧
    isDiscovery (workspaceRootRecovery .agentUnavailable wv) := by
  cases op <;> cases r <;> cases d <;> cases v <;> cases wv <;> decide

end OperationRecovery
