import Proofs.CanonicalOutput.Message
import Proofs.Workspace.ChildResolution
import Proofs.Background.State

namespace CanonicalOutput

abbrev DelegatedWorkspace := Workspace.ChildStamp

/-- The addressed bridge copies the accepted parent stamp exactly. A child
workspace request is resolved later by `Workspace.resolveChild`. -/
def receiveDelegatedWorkspace (source copied : Option DelegatedWorkspace) : Bool :=
  source == copied

structure DelegatedInput where
  source : PayloadRef
  arguments : String
  parentSubagentDepth : Nat
  deriving DecidableEq, Repr

inductive DelegationError where
  | invalidPublication
  | localTarget
  | unknownCall
  | invalidSource
  | message (error : MessageError)
  | reconstruction (error : ReconstructionError)
  deriving DecidableEq, Repr

def isExecutionPublication : MessagePublication → Bool
  | .requestExecution _ => true
  | _ => false

def isCompleteProviderSource (closing : Segment) : Bool :=
  match closing.coordinate.source, closing.close with
  | .provider _ _ _, some (.closed .complete _ _) => true
  | _, _ => false

/-- Coordinator-side admission. This runs inside the accepted publication
transaction, not on the remote host. Only the addressed call's bytes are copied;
the returned source reference is provenance and grants no parent read access. -/
def prepareDelegatedInput (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) (intent : ToolIntent) (parentSubagentDepth : Nat) :
    Except DelegationError DelegatedInput := do
  if message.header.outcome != .complete || message.header.role != .assistant then
    .error .invalidPublication
  else if !isExecutionPublication message.header.publication then .error .invalidPublication
  else if intent ∉ toolIntents message then .error .unknownCall
  else
    let _ ← (reconstructMessage records denied message).mapError DelegationError.message
    let closing ← (resolveClose records denied intent.arguments).mapError
      (fun error => DelegationError.reconstruction (.lookup error))
    if message.header.request != some closing.coordinate.request ||
        !isCompleteProviderSource closing then .error .invalidSource
    else
      let (declaration, bytes) ← (reconstructPayload records denied intent.arguments).mapError
        DelegationError.reconstruction
      if declaration.kind != .arguments then .error .invalidSource
      else match utf8? bytes with
        | none => .error .invalidSource
        | some arguments => .ok ⟨intent.arguments, arguments, parentSubagentDepth⟩

/-- The accepted row is addressed under existing coordinator document ACP.
There are no parent header/segment dependencies in this transport value. -/
structure DelegatedCall where
  call : DocId
  coordinator : Nat
  target : Nat
  behavior : Nat
  input : DelegatedInput
  workspace : Option DelegatedWorkspace
  deriving DecidableEq, Repr

def receiveDelegatedInput (authenticatedCoordinator host configuredBehavior : Nat)
    (row : DelegatedCall) : Option DelegatedInput :=
  if row.coordinator = authenticatedCoordinator ∧ row.target = host ∧
      row.behavior = configuredBehavior ∧ row.coordinator ≠ row.target then some row.input else none

/-- The trusted paired peer consumes the copied depth at child creation. The
bound is the existing subagent owner, not a second delegation limit. -/
def materializeDelegatedChildDepth (input : DelegatedInput) : Option Nat :=
  if input.parentSubagentDepth < Subagent.maxSubagentDepth then
    some (input.parentSubagentDepth + 1)
  else none

def receiveDelegatedChildDepth (authenticatedCoordinator host configuredBehavior : Nat)
    (row : DelegatedCall) : Option Nat :=
  (receiveDelegatedInput authenticatedCoordinator host configuredBehavior row).bind
    materializeDelegatedChildDepth

/-- Host materialization composes the copied parent stamp with the existing
workspace resolver; the requested child workspace is never the copied stamp. -/
def receiveDelegatedChild (authenticatedCoordinator host configuredBehavior
    parentAgent childAgent : Nat) (row : DelegatedCall)
    (choice : Workspace.ChildChoice) : Option (Nat × Option DelegatedWorkspace) := do
  let input ← receiveDelegatedInput authenticatedCoordinator host configuredBehavior row
  let depth ← materializeDelegatedChildDepth input
  let workspace ← Workspace.resolveChild row.workspace parentAgent childAgent choice
  some (depth, workspace)

theorem materialized_child_depth_bounded (input : DelegatedInput) (child : Nat)
    (h : materializeDelegatedChildDepth input = some child) :
    child = input.parentSubagentDepth + 1 ∧ child ≤ Subagent.maxSubagentDepth := by
  unfold materializeDelegatedChildDepth at h
  split at h
  · cases h
    constructor
    · rfl
    · omega
  · contradiction

theorem received_child_depth_and_authority_bounded
    (coordinator host behavior parentAgent childAgent : Nat)
    (row : DelegatedCall) (choice : Workspace.ChildChoice)
    (parent child : DelegatedWorkspace) (depth : Nat)
    (hp : row.workspace = some parent)
    (h : receiveDelegatedChild coordinator host behavior parentAgent childAgent row choice =
      some (depth, some child)) :
    depth ≤ Subagent.maxSubagentDepth ∧
      Workspace.authorityRank child.authority ≤ Workspace.authorityRank parent.authority := by
  unfold receiveDelegatedChild at h
  cases hi : receiveDelegatedInput coordinator host behavior row with
  | none => simp [hi] at h
  | some input =>
      simp only [hi, Option.bind_some] at h
      cases hd : materializeDelegatedChildDepth input with
      | none => simp [hd] at h
      | some actualDepth =>
          cases hw : Workspace.resolveChild row.workspace parentAgent childAgent choice with
          | none => simp [hw] at h
          | some actualWorkspace =>
              simp [hi, hd, hw] at h
              rcases h with ⟨rfl, rfl⟩
              exact ⟨(materialized_child_depth_bounded input _ hd).2,
                Workspace.resolved_child_cannot_exceed_parent parent child
                  parentAgent childAgent choice (by simpa [hp] using hw)⟩

theorem received_readonly_parent_cannot_escalate
    (coordinator host behavior parentAgent childAgent : Nat)
    (row : DelegatedCall) (choice : Workspace.ChildChoice)
    (parent child : DelegatedWorkspace) (depth : Nat)
    (hp : row.workspace = some parent) (ha : parent.authority = .readOnly)
    (h : receiveDelegatedChild coordinator host behavior parentAgent childAgent row choice =
      some (depth, some child)) : child.authority = .readOnly := by
  have bound := (received_child_depth_and_authority_bounded coordinator host behavior
    parentAgent childAgent row choice parent child depth hp h).2
  cases hc : child.authority <;> simp [Workspace.authorityRank, ha, hc] at bound ⊢

/-- The coordinator binds the copied argument bytes to the one addressed remote
call. Local calls retain no delegated projection. -/
def prepareDelegatedCall (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) (intent : ToolIntent) (coordinator target behavior
    parentSubagentDepth : Nat) (workspace : Option DelegatedWorkspace) :
    Except DelegationError DelegatedCall := do
  if coordinator = target then .error .localTarget
  else
    let input ← prepareDelegatedInput records denied message intent parentSubagentDepth
    .ok ⟨intent.call, coordinator, target, behavior, input, workspace⟩

theorem wrong_target_cannot_receive (coordinator host : Nat) (row : DelegatedCall)
    (h : row.target ≠ host) : receiveDelegatedInput coordinator host row.behavior row = none := by
  simp [receiveDelegatedInput, h]

theorem local_call_has_no_delegated_input (principal call configuredBehavior : Nat)
    (input : DelegatedInput) :
    receiveDelegatedInput principal principal configuredBehavior
      ⟨call, principal, principal, configuredBehavior, input, none⟩ = none := by
  simp [receiveDelegatedInput]

theorem source_reference_is_not_a_hydration_root
    (coordinator host call configuredBehavior : Nat) (input : DelegatedInput)
    (hremote : coordinator ≠ host) :
    receiveDelegatedInput coordinator host configuredBehavior
      ⟨call, coordinator, host, configuredBehavior, input, none⟩ = some input := by
  simp [receiveDelegatedInput, hremote]

theorem admitted_input_is_exact_argument_stream
    (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) (intent : ToolIntent) (parentSubagentDepth : Nat)
    (input : DelegatedInput)
    (h : prepareDelegatedInput records denied message intent parentSubagentDepth = .ok input) :
    input.source = intent.arguments ∧
      ∃ declaration bytes, reconstructPayload records denied intent.arguments =
        .ok (declaration, bytes) ∧ utf8? bytes = some input.arguments ∧
          declaration.kind = .arguments ∧ input.parentSubagentDepth = parentSubagentDepth := by
  unfold prepareDelegatedInput at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · split at h
      · contradiction
      · cases hm : reconstructMessage records denied message with
        | error error => simp [hm, Except.mapError, Bind.bind, Except.bind] at h
        | ok native =>
          simp only [hm, Except.mapError, Bind.bind, Except.bind] at h
          cases hc : resolveClose records denied intent.arguments with
          | error error => simp [hc, Except.mapError, Bind.bind, Except.bind] at h
          | ok closing =>
            simp only [hc, Except.mapError, Bind.bind, Except.bind] at h
            split at h
            · contradiction
            · cases hp : reconstructPayload records denied intent.arguments with
              | error error => simp [hp, Except.mapError, Bind.bind, Except.bind] at h
              | ok pair =>
                rcases pair with ⟨declaration, bytes⟩
                simp only [hp, Except.mapError, Bind.bind, Except.bind] at h
                split at h
                · contradiction
                · rename_i hkind
                  cases hutf8 : utf8? bytes with
                  | none => simp [hutf8] at h
                  | some arguments =>
                      simp only [hutf8, Except.ok.injEq] at h
                      cases h
                      refine ⟨rfl, declaration, bytes, rfl, hutf8, ?_⟩
                      exact ⟨by simpa using hkind, rfl⟩

end CanonicalOutput
