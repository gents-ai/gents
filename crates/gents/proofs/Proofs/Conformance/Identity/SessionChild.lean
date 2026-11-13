import Proofs.Store.Identity.SessionChild
import Proofs.Conformance.Family

namespace Conformance.SessionChild

open Lean Identity.SessionChild

structure ChildScopeInput where
  name : String
  binding : ChildScopeBinding
  nativeOwner : Option String
  creator : Option String
  authenticated : Option String
  grantOk : Bool
  markOk : Bool
  committed : Bool
  initialParentGranted : Bool
  initialParentLinked : Bool
  deriving ToJson

def ChildScopeInput.ofCase (c : ChildScopeCase) : ChildScopeInput :=
  { name := c.name, binding := c.binding, nativeOwner := c.nativeOwner, creator := c.creator
    authenticated := c.authenticated, grantOk := c.grantOk, markOk := c.markOk
    committed := c.committed, initialParentGranted := c.parentGranted
    initialParentLinked := c.parentLinked }

structure BindingOutput where
  allowed : Bool
  deriving ToJson

structure RepairOutput where
  parentGranted : Bool
  parentLinked : Bool
  deriving ToJson

def repaired (i : ChildScopeInput) : RepairOutput :=
  let result := recoverBoundSessionChild ⟨i.committed, i.initialParentGranted, i.initialParentLinked⟩
    i.binding i.nativeOwner i.creator i.authenticated i.grantOk i.markOk
  { parentGranted := result.parentGranted, parentLinked := result.parentLinked }

def servedBindingNames : List String :=
  ["paired_title_segment", "paired_title_capture", "paired_review_segment",
    "paired_review_capture", "ordinary_same_requester", "ordinary_foreign_requester",
    "ordinary_absent_requester", "source_kind_mismatch", "foreign_actor", "foreign_child_node",
    "foreign_child_session", "foreign_child_requester", "missing_auxiliary",
    "foreign_auxiliary_document"]

def servedRepairNames : List String :=
  ["paired_title_segment", "missing_parent", "invalid_parent_signature", "foreign_native_owner",
    "completed_then_revoked"]

/-- A served node writes the session, a signed parent and a pending auxiliary
request through their production owners, so only the child's own node,
session, requester, actor, source and request document can be varied; the
signed rows' contents cannot be forged on that fixture. -/
@[conformance_family]
def childScopeBinding : Family ChildScopeInput BindingOutput where
  inputs := (childScopeCases.filter fun c => c.name ∈ servedBindingNames).map .ofCase
  model := fun i => { allowed := childScopeBindingAllowed i.binding }

/-- model-production-gap: the served binding fixture cannot write a session,
parent or auxiliary request whose signed contents, owner fields or collection
differ from what their production writers produce. -/
@[conformance_family]
def childScopeBindingUnreached : Family ChildScopeInput BindingOutput where
  inputs := (childScopeCases.filter fun c => c.name ∉ servedBindingNames).map .ofCase
  model := fun i => { allowed := childScopeBindingAllowed i.binding }

/-- Repair runs on a served node where the auxiliary request, its parent and
the child segment are written by their signers; the scenario can omit or
tamper the parent, create the child as another Agent, or complete and then
revoke the link, and nothing else. -/
@[conformance_family]
def childScopeRepair : Family ChildScopeInput RepairOutput where
  inputs := (childScopeCases.filter fun c => c.name ∈ servedRepairNames).map .ofCase
  model := repaired

/-- model-production-gap: native grant or marker failures, uncommitted
children and forged binding fields cannot be injected into the served repair
fixture. -/
@[conformance_family]
def childScopeRepairUnreached : Family ChildScopeInput RepairOutput where
  inputs := (childScopeCases.filter fun c => c.name ∉ servedRepairNames).map .ofCase
  model := repaired

structure ClientRequestInput where
  name : String
  committed : Bool
  initialPending : Bool
  matchingSignature : Bool
  sameSigner : Bool
  grantsOk : Bool
  markOk : Bool
  deriving ToJson

structure ClientRequestOutput where
  pending : Bool
  grants : Bool
  deriving ToJson

def ClientRequestInput.ofCase (c : ClientRequestCase) : ClientRequestInput :=
  { name := c.name, committed := c.initial.committed, initialPending := c.initial.pending
    matchingSignature := c.matchingSignature, sameSigner := c.sameSigner, grantsOk := c.grantsOk
    markOk := c.markOk }

def recoveredClientRequest (i : ClientRequestInput) : ClientRequestOutput :=
  let result := recoverClientRequest ⟨i.committed, i.initialPending, false⟩ i.matchingSignature
    i.sameSigner i.grantsOk i.markOk
  { pending := result.pending, grants := result.grants }

example : clientRequestCases.all fun c => !c.initial.grants := by decide

example : (clientRequestCases.find? (·.name == "wrong_signer")).map
    (fun c => (recoverClientRequest c.initial c.matchingSignature c.sameSigner c.grantsOk c.markOk).pending) =
    some true := by decide

def servedClientRequestNames : List String := ["wrong_signer", "grant_succeeded"]

/-- One served retry fixture holds a committed, pending client request: another
actor's recovery leaves it untouched, then its signer recovers it, in that
order. Grant or marker failures, a mismatched signature and a completed
publication cannot be produced on that fixture. -/
@[conformance_family]
def clientRequestRecovery : Family ClientRequestInput ClientRequestOutput where
  inputs := servedClientRequestNames.filterMap fun name =>
    (clientRequestCases.find? (·.name == name)).map .ofCase
  model := recoveredClientRequest

/-- model-production-gap: the served retry fixture cannot fail the working
Agent grant or the marker write, sign a mismatched request, or start from a
completed publication whose grant was revoked. -/
@[conformance_family]
def clientRequestRecoveryUnreached : Family ClientRequestInput ClientRequestOutput where
  inputs := (clientRequestCases.filter fun c => c.name ∉ servedClientRequestNames).map .ofCase
  model := recoveredClientRequest

end Conformance.SessionChild
