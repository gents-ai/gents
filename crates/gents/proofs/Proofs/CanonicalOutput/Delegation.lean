import Proofs.CanonicalOutput.Message
import Proofs.Workspace.Types

namespace CanonicalOutput

structure DelegatedWorkspace where
  workspaceId : Nat
  ownerAgent : Nat
  sealHash : Option Nat
  authority : BindingAuthority
  deriving DecidableEq, Repr

def workspaceAuthorityRank : BindingAuthority → Nat
  | .readOnly => 0
  | .integrate => 1
  | .readWrite => 2

def workspaceAttenuates (source child : DelegatedWorkspace) : Bool :=
  child.workspaceId == source.workspaceId && child.ownerAgent == source.ownerAgent &&
    child.sealHash == source.sealHash &&
    workspaceAuthorityRank child.authority <= workspaceAuthorityRank source.authority

def receiveDelegatedWorkspace (source child : Option DelegatedWorkspace) : Bool :=
  match source, child with
  | none, none => true
  | some parent, some requested => workspaceAttenuates parent requested
  | _, _ => false

theorem delegated_workspace_cannot_escalate_readonly
    (source child : DelegatedWorkspace)
    (hsource : source.authority = .readOnly)
    (h : workspaceAttenuates source child = true) :
    child.authority = .readOnly := by
  cases ha : child.authority <;>
    simp [workspaceAttenuates, workspaceAuthorityRank, hsource, ha] at h ⊢

structure DelegatedInput where
  source : PayloadRef
  arguments : String
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
    (message : MessageEnvelope) (intent : ToolIntent) :
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
        | some arguments => .ok ⟨intent.arguments, arguments⟩

/-- The accepted row is addressed under existing coordinator document ACP.
There are no parent header/segment dependencies in this transport value. -/
structure DelegatedCall where
  call : DocId
  coordinator : Nat
  target : Nat
  behavior : Nat
  input : DelegatedInput
  deriving DecidableEq, Repr

def receiveDelegatedInput (authenticatedCoordinator host configuredBehavior : Nat)
    (row : DelegatedCall) : Option DelegatedInput :=
  if row.coordinator = authenticatedCoordinator ∧ row.target = host ∧
      row.behavior = configuredBehavior ∧ row.coordinator ≠ row.target then some row.input else none

/-- The coordinator binds the copied argument bytes to the one addressed remote
call. Local calls retain no delegated projection. -/
def prepareDelegatedCall (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) (intent : ToolIntent) (coordinator target behavior : Nat) :
    Except DelegationError DelegatedCall := do
  if coordinator = target then .error .localTarget
  else
    let input ← prepareDelegatedInput records denied message intent
    .ok ⟨intent.call, coordinator, target, behavior, input⟩

theorem wrong_target_cannot_receive (coordinator host : Nat) (row : DelegatedCall)
    (h : row.target ≠ host) : receiveDelegatedInput coordinator host row.behavior row = none := by
  simp [receiveDelegatedInput, h]

theorem local_call_has_no_delegated_input (principal call : Nat) (input : DelegatedInput) :
    receiveDelegatedInput principal principal 7 ⟨call, principal, principal, 7, input⟩ = none := by
  simp [receiveDelegatedInput]

theorem source_reference_is_not_a_hydration_root
    (coordinator host call : Nat) (input : DelegatedInput) (hremote : coordinator ≠ host) :
    receiveDelegatedInput coordinator host 7 ⟨call, coordinator, host, 7, input⟩ = some input := by
  simp [receiveDelegatedInput, hremote]

theorem admitted_input_is_exact_argument_stream
    (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) (intent : ToolIntent) (input : DelegatedInput)
    (h : prepareDelegatedInput records denied message intent = .ok input) :
    input.source = intent.arguments ∧
      ∃ declaration bytes, reconstructPayload records denied intent.arguments =
        .ok (declaration, bytes) ∧ utf8? bytes = some input.arguments ∧
          declaration.kind = .arguments := by
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
                      simpa using hkind

end CanonicalOutput
