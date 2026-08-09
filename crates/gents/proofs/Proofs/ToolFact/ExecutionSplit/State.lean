import Proofs.ToolFact.State

/-!
# Split tool invocation, execution, output, and approval state

The execution store is keyed by composite commit CID rather than document ID.
This deliberately retains historical execution versions: an output can pin the
running version and a terminal version can later pin that output without making
the graph cyclic.
-/

namespace ToolFact.ExecutionSplit

open RenderedCapture
open ToolFact

inductive ExecutionPhase where
  | pending
  | awaitingApproval
  | running
  | completed
  | failed
  | timedOut
  | cancelled
  deriving DecidableEq, Repr

def ExecutionPhase.toContract : ExecutionPhase → String
  | .pending => "pending"
  | .awaitingApproval => "awaiting_approval"
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .timedOut => "timed_out"
  | .cancelled => "cancelled"

inductive OmissionReason where
  | preDispatchFailure
  | approvalDenied
  | executionLost
  | recoveryFailure
  | childDead
  | childSuperseded
  | timedOut
  | cancelled
  deriving DecidableEq, Repr

def OmissionReason.terminalPhase : OmissionReason → ExecutionPhase
  | .preDispatchFailure => .failed
  | .approvalDenied => .failed
  | .executionLost => .failed
  | .recoveryFailure => .failed
  | .childDead => .failed
  | .childSuperseded => .failed
  | .timedOut => .timedOut
  | .cancelled => .cancelled

def OmissionReason.toContract : OmissionReason → String
  | .preDispatchFailure => "pre_dispatch_failure"
  | .approvalDenied => "approval_denied"
  | .executionLost => "execution_lost"
  | .recoveryFailure => "recovery_failure"
  | .childDead => "child_dead"
  | .childSuperseded => "child_superseded"
  | .timedOut => "timed_out"
  | .cancelled => "cancelled"

structure ToolInvocationFact where
  key : LogicalKey
  signed : SignedRef
  argsHash : PayloadHash
  deriving DecidableEq, Repr

structure ToolInvocationIntent where
  key : LogicalKey
  argsHash : PayloadHash
  deriving DecidableEq, Repr

structure ToolExecutionFact where
  signed : SignedRef
  invocation : SignedRef
  ownerDid : SignerDid
  epoch : Nat
  phase : ExecutionPhase
  previous : Option SignedRef := none
  approval : Option SignedRef := none
  output : Option SignedRef := none
  omission : Option SignedRef := none
  deriving DecidableEq, Repr

structure ToolExecutionIntent where
  invocation : SignedRef
  ownerDid : SignerDid
  epoch : Nat
  phase : ExecutionPhase
  deriving DecidableEq, Repr

structure ToolOutputFact where
  key : LogicalKey
  signed : SignedRef
  invocation : SignedRef
  execution : SignedRef
  outputHash : PayloadHash
  fullOutput : Bool
  deriving DecidableEq, Repr

structure ToolOutputIntent where
  key : LogicalKey
  invocation : SignedRef
  execution : SignedRef
  outputHash : PayloadHash
  fullOutput : Bool
  deriving DecidableEq, Repr

structure ToolApprovalFact where
  key : LogicalKey
  signed : SignedRef
  invocation : SignedRef
  execution : SignedRef
  decision : ApprovalDecision
  reasonHash : PayloadHash
  deriving DecidableEq, Repr

structure ToolApprovalIntent where
  key : LogicalKey
  invocation : SignedRef
  execution : SignedRef
  decision : ApprovalDecision
  reasonHash : PayloadHash
  deriving DecidableEq, Repr

structure ToolOutputOmissionFact where
  key : LogicalKey
  signed : SignedRef
  invocation : SignedRef
  execution : SignedRef
  reason : OmissionReason
  /-- Present only for `approvalDenied`, pinning the exact denied decision. -/
  approval : Option SignedRef := none
  deriving DecidableEq, Repr

structure ToolOutputOmissionIntent where
  key : LogicalKey
  invocation : SignedRef
  execution : SignedRef
  reason : OmissionReason
  approval : Option SignedRef := none
  deriving DecidableEq, Repr

abbrev InvocationStore := Store ToolInvocationFact
/-- Historical execution versions indexed by composite commit CID. -/
abbrev ExecutionStore := Store ToolExecutionFact
abbrev OutputStore := Store ToolOutputFact
abbrev ApprovalStore := Store ToolApprovalFact
abbrev OmissionStore := Store ToolOutputOmissionFact

structure State where
  invocations : InvocationStore
  executions : ExecutionStore
  outputs : OutputStore
  approvals : ApprovalStore
  omissions : OmissionStore

def State.empty : State :=
  { invocations := Store.empty
  , executions := Store.empty
  , outputs := Store.empty
  , approvals := Store.empty
  , omissions := Store.empty }

def ToolInvocationFact.forIntent
    (intent : ToolInvocationIntent) (signed : SignedRef) : ToolInvocationFact :=
  { key := intent.key, signed := signed, argsHash := intent.argsHash }

def ToolExecutionFact.genesis
    (intent : ToolExecutionIntent) (signed : SignedRef) : ToolExecutionFact :=
  { signed := signed
  , invocation := intent.invocation
  , ownerDid := intent.ownerDid
  , epoch := intent.epoch
  , phase := intent.phase }

def ToolOutputFact.forIntent
    (intent : ToolOutputIntent) (signed : SignedRef) : ToolOutputFact :=
  { key := intent.key
  , signed := signed
  , invocation := intent.invocation
  , execution := intent.execution
  , outputHash := intent.outputHash
  , fullOutput := intent.fullOutput }

def ToolApprovalFact.forIntent
    (intent : ToolApprovalIntent) (signed : SignedRef) : ToolApprovalFact :=
  { key := intent.key
  , signed := signed
  , invocation := intent.invocation
  , execution := intent.execution
  , decision := intent.decision
  , reasonHash := intent.reasonHash }

def ToolOutputOmissionFact.forIntent
    (intent : ToolOutputOmissionIntent) (signed : SignedRef) : ToolOutputOmissionFact :=
  { key := intent.key
  , signed := signed
  , invocation := intent.invocation
  , execution := intent.execution
  , reason := intent.reason
  , approval := intent.approval }

def exactInvocation? (store : InvocationStore) (ref : SignedRef) : Option ToolInvocationFact :=
  match store ref.version.docId with
  | some fact =>
      if fact.signed = ref ∧ ref.authoritative = true then some fact else none
  | none => none

def exactExecution? (store : ExecutionStore) (ref : SignedRef) : Option ToolExecutionFact :=
  match store ref.version.compositeCommitCid with
  | some fact =>
      if fact.signed = ref ∧ ref.authoritative = true then some fact else none
  | none => none

def exactOutput? (store : OutputStore) (ref : SignedRef) : Option ToolOutputFact :=
  match store ref.version.docId with
  | some fact =>
      if fact.signed = ref ∧ ref.authoritative = true ∧ fact.fullOutput = true then
        some fact
      else none
  | none => none

def exactApproval? (store : ApprovalStore) (ref : SignedRef) : Option ToolApprovalFact :=
  match store ref.version.docId with
  | some fact =>
      if fact.signed = ref ∧ ref.authoritative = true then some fact else none
  | none => none

def exactOmission? (store : OmissionStore) (ref : SignedRef) : Option ToolOutputOmissionFact :=
  match store ref.version.docId with
  | some fact =>
      if fact.signed = ref ∧ ref.authoritative = true then some fact else none
  | none => none

end ToolFact.ExecutionSplit
