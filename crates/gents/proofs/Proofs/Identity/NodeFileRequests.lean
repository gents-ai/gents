import Lean
import Proofs.Identity.Household

namespace Identity.NodeFileRequests

/-- Retained request schemas do not have a production publisher or consumer in
the direct-HTTP inventory flow. Their scope names a node, and their receipt
signer is the target node; none contains a physical session or selected Agent.
If used, the authenticated creator owns the request. A Node parent and explicit
runtime writer must commit with creation; the payload fields grant no access. -/
structure Publication where
  document : Bool := false
  nodeParent : Bool := false
  runtimeWriter : Bool := false
  deriving BEq, DecidableEq, Repr

def publish (committed : Bool) : Publication :=
  if committed then ⟨true, true, true⟩ else ⟨false, false, false⟩

def revokeRuntimeWriter (p : Publication) : Publication := { p with runtimeWriter := false }
def observeExisting (p : Publication) : Publication := p

theorem aborted_publication_has_no_partial_relationships : publish false = ⟨false, false, false⟩ := by rfl
theorem duplicate_does_not_restore_revoked_relationship :
    observeExisting (revokeRuntimeWriter (publish true)) = ⟨true, true, false⟩ := by rfl

def requestFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "request", "user"⟩],
    relations := [⟨"configuration", "request", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "request", "writer", .actor "runtime"⟩], enrolled := [] }

def requestPermission (actor permission : String) : Bool :=
  Household.allowed 5 requestFixture actor "configuration" "request" permission

theorem requester_and_explicit_runtime_have_native_access :
    requestPermission "user" "read" = true ∧ requestPermission "user" "update" = true ∧
    requestPermission "runtime" "read" = true ∧ requestPermission "runtime" "update" = true ∧
    requestPermission "other" "read" = false ∧ requestPermission "other" "update" = false := by native_decide

def revokedRuntimeFixture : Household.State :=
  Household.apply requestFixture (.revoke ⟨"configuration", "request", "writer", .actor "runtime"⟩)

theorem runtime_access_requires_its_explicit_grant :
    Household.allowed 5 revokedRuntimeFixture "runtime" "configuration" "request" "read" = false ∧
    Household.allowed 5 revokedRuntimeFixture "runtime" "configuration" "request" "update" = false := by native_decide

def requestAccessCasesJson : String :=
  (Lean.toJson (["user", "runtime", "agent", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => Lean.Json.mkObj
      [("actor", Lean.toJson actor), ("permission", Lean.toJson permission),
       ("expected", Lean.toJson (requestPermission actor permission)),
       ("revoked_expected", Lean.toJson (Household.allowed 5 revokedRuntimeFixture actor "configuration" "request" permission)),
       ("aborted_document", Lean.toJson (publish false).document)])).compress


end Identity.NodeFileRequests
