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

/-- Every case: the fixture writes the session and signed parent as the User,
the auxiliary request as the Agent named as its signer (forged fields and
signatures included, as a compromised client or runtime holding those keys can
write them), then asks the binding as the named actor. -/
@[conformance_family]
def childScopeBinding : Family ChildScopeInput BindingOutput where
  inputs := childScopeCases.map .ofCase
  model := fun i => { allowed := childScopeBindingAllowed i.binding }

/-- Repair evaluates the binding inside the authenticated repairer's own
transaction, so the binding's actor is the authenticated creator; a case that
separates them is a binding case only. -/
@[conformance_family]
def childScopeRepair : Family ChildScopeInput RepairOutput where
  inputs := (childScopeCases.filter fun c =>
    c.authenticated == some c.binding.actor).map .ofCase
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

/-- The retry fixture holds one committed, signed client retry per case in one
session: its signer recovers one while the working Agent grant staged with the
marker clear fails, another actor recovers one, its signer recovers one, its
signer recovers one whose admission signature does not verify, and its signer
recovers one it completed and whose working Agent grant it then revoked. -/
@[conformance_family]
def clientRequestRecovery : Family ClientRequestInput ClientRequestOutput where
  inputs := clientRequestCases.map .ofCase
  model := recoveredClientRequest

deriving instance ToJson for Identity.SessionChild.ClientRequestPublication

structure AtomicRequestInput where
  name : String
  before : ClientRequestPublication
  fresh : Bool
  commit : Bool
  deriving ToJson

def atomicRequestCases : List AtomicRequestInput :=
  [ { name := "aborted", before := ⟨false, false, false⟩, fresh := true, commit := false }
  , { name := "created", before := ⟨false, false, false⟩, fresh := true, commit := true }
  , { name := "duplicate-after-revocation", before := ⟨true, false, false⟩, fresh := false,
      commit := true } ]

/-- One signed client request on one served node: a publication aborted after
its relationships were staged, the committed publication, then a duplicate
after the working Agent grant was revoked, in that order. -/
@[conformance_family]
def atomicClientRequest : Family AtomicRequestInput ClientRequestPublication where
  inputs := atomicRequestCases
  model := fun i => commitFreshClientRequest i.before i.fresh i.commit

/-- A lost commit receipt leaves a committed fresh publication. The receipt-loss
fixture submits one fresh request and loses only its receipt, so only the
committed fresh case can be built there. -/
@[conformance_family]
def atomicClientRequestUnconfirmed : Family AtomicRequestInput ClientRequestPublication where
  inputs := atomicRequestCases.filter (·.name == "created")
  model := fun i => commitFreshClientRequest i.before i.fresh i.commit

/-- A client-requested Task delivery publishes the same signed client request
through the Task delivery owner, whose staging step is private to it: the
scenario delivers the request, then delivers it again after the working Agent
grant was revoked, and cannot abort between staging and commit. -/
@[conformance_family]
def atomicClientTaskRequest : Family AtomicRequestInput ClientRequestPublication where
  inputs := atomicRequestCases.filter (·.commit)
  model := fun i => commitFreshClientRequest i.before i.fresh i.commit

end Conformance.SessionChild
