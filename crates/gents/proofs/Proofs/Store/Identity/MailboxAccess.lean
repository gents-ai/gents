import Lean
import Proofs.Store.Identity.Household

namespace Identity.MailboxAccess

/-- A mailbox item can lack a session. Its native creator is the held working
Agent, and the authenticated recipient gets an explicit relation for reading
and resolving the item. An item payload's requester DID alone is not proof of
the recipient; provenance must come from the existing authenticated request
or session owner. This candidate does not itself bind a collection. -/
def candidatePolicy : List Household.ResourceDefinition :=
  [{ name := "mailbox_items",
     relations := [⟨"recipient", ["actor"], []⟩],
     permissions := [Household.permission "read" ["recipient"] [],
       Household.permission "update" ["recipient"] [], Household.permission "delete" [] []] }]

def fixture : Household.State :=
  { objects := [⟨"mailbox_items", "item", "agent"⟩],
    relations := [⟨"mailbox_items", "item", "recipient", .actor "user"⟩], enrolled := [] }

def permitted (state : Household.State) (actor permission : String) : Bool :=
  Household.allowedIn candidatePolicy 5 state actor "mailbox_items" "item" permission

theorem creator_and_recipient_can_read_and_update :
    permitted fixture "agent" "read" = true ∧ permitted fixture "agent" "update" = true ∧
    permitted fixture "user" "read" = true ∧ permitted fixture "user" "update" = true ∧
    permitted fixture "other" "read" = false ∧ permitted fixture "other" "update" = false ∧
    permitted fixture "runtime" "read" = false := by native_decide

def revoked : Household.State :=
  Household.apply fixture (.revoke ⟨"mailbox_items", "item", "recipient", .actor "user"⟩)

theorem revocation_removes_recipient_access :
    permitted revoked "user" "read" = false ∧ permitted revoked "user" "update" = false := by native_decide

/-- A native transaction publishes the item and recipient relationship
together. An invalid or unavailable source identity yields no publication;
the node DID is not a replacement user. An existing item is not a new grant,
including after deliberate revocation. -/
structure Publication where
  document : Bool := false
  recipient : Bool := false
  deriving BEq, DecidableEq, Repr

def publish (heldAgent verifiedRecipient committed : Bool) : Publication :=
  if heldAgent && verifiedRecipient && committed then ⟨true, true⟩ else ⟨false, false⟩

def revokeRecipient (p : Publication) : Publication := { p with recipient := false }
def observeExisting (p : Publication) : Publication := p

theorem unavailable_recipient_leaves_no_item : publish true false true = ⟨false, false⟩ := by rfl
theorem failed_commit_leaves_no_partial_item : publish true true false = ⟨false, false⟩ := by rfl
theorem duplicate_does_not_regrant_recipient :
    observeExisting (revokeRecipient (publish true true true)) = ⟨true, false⟩ := by rfl

/-- Runtime reconciliation observes each item's selected held target Agent.
Invisible items are not inferred from the node or another Agent's access. -/
def sweepVisible (held target : String) : Bool := held == target

theorem runtime_node_is_not_mailbox_target :
    sweepVisible "node" "agent" = false ∧ sweepVisible "agent" "agent" = true := by decide

def sweepActorCasesJson : String :=
  (Lean.toJson (["agent", "other", "node"].map fun actor => Lean.Json.mkObj
    [("actor", Lean.toJson actor), ("visible", Lean.toJson (sweepVisible actor "agent"))])).compress

/-- Interactive recipient provenance is retained from the verified signed
request when its native claim commits. Physical targets belong to that same
signed record; a session owner or a logical request label cannot substitute
for the admitted signer. Subsequent permission changes do not retract a claim. -/
structure AdmittedRequest where
  signer : String
  requestDoc : String
  sessionDoc : String
  agentDoc : String
  deriving BEq, DecidableEq, Repr

def claimedRecipient (admitted : Option AdmittedRequest) (claimCommitted : Bool) :
    Option AdmittedRequest :=
  if claimCommitted then admitted else none

theorem admitted_delegate_is_recipient :
    claimedRecipient (some ⟨"delegate", "request", "session", "agent"⟩) true =
      some ⟨"delegate", "request", "session", "agent"⟩ := by rfl

theorem uncommitted_claim_has_no_recipient :
    claimedRecipient (some ⟨"delegate", "request", "session", "agent"⟩) false = none := by rfl

def claimedRecipientCasesJson : String :=
  (Lean.toJson ([false, true].flatMap fun present => [false, true].map fun committed =>
    let record : Option AdmittedRequest := if present then
      some ⟨"delegate", "request", "session", "agent"⟩ else none
    let result := claimedRecipient record committed
    Lean.Json.mkObj [("present", Lean.toJson present),
      ("committed", Lean.toJson committed),
      ("signer", Lean.toJson (result.map (·.signer))),
      ("request_doc", Lean.toJson (result.map (·.requestDoc))),
      ("session_doc", Lean.toJson (result.map (·.sessionDoc))),
      ("agent_doc", Lean.toJson (result.map (·.agentDoc)))] )).compress

def nativeCasesJson : String :=
  (Lean.Json.mkObj [
    ("policy_yaml", Lean.toJson (Household.policyYamlOf candidatePolicy)),
    ("aborted_document", Lean.toJson (publish true true false).document),
    ("checks", Lean.toJson (["agent", "user", "other", "runtime"].flatMap fun actor =>
      ["read", "update", "delete"].map fun permission => Lean.Json.mkObj
        [("actor", Lean.toJson actor), ("permission", Lean.toJson permission),
         ("expected", Lean.toJson (permitted fixture actor permission)),
         ("revoked_expected", Lean.toJson (permitted revoked actor permission))]))]).compress

/-- A restart notice names its request only by a logical label in a durable
source receipt. The label locates the request's signed record as its held
target Agent sees it; the recipient is that record's claimed recipient, so a
session field or label never establishes the requester. Without a committed
claim the notice is deferred and its durable source receipt remains available
for later repair. -/
def runtimeNotice (admitted : Option AdmittedRequest) (claimCommitted committed : Bool) :
    Publication :=
  publish true (claimedRecipient admitted claimCommitted).isSome committed

def runtimeNoticeWithoutClaim : Publication :=
  runtimeNotice (some ⟨"delegate", "request", "session", "agent"⟩) false true

theorem unavailable_runtime_notice_has_no_publication :
    runtimeNoticeWithoutClaim = ⟨false, false⟩ := by rfl

theorem claimed_runtime_notice_reaches_admitted_signer :
    runtimeNotice (some ⟨"delegate", "request", "session", "agent"⟩) true true = ⟨true, true⟩ := by rfl

theorem missing_request_runtime_notice_has_no_publication :
    runtimeNotice none true true = ⟨false, false⟩ := by rfl

private def runtimeNoticeJson (p : Publication) : Lean.Json :=
  Lean.Json.mkObj [("document", Lean.toJson p.document),
    ("recipient", Lean.toJson p.recipient), ("deferred", Lean.toJson (!p.document))]

def runtimeNoticeCasesJson : String :=
  (Lean.Json.mkObj [("document", Lean.toJson runtimeNoticeWithoutClaim.document),
    ("recipient", Lean.toJson runtimeNoticeWithoutClaim.recipient),
    ("deferred", Lean.toJson (!runtimeNoticeWithoutClaim.document)),
    ("claimed", runtimeNoticeJson
      (runtimeNotice (some ⟨"delegate", "request", "session", "agent"⟩) true true))]).compress

end Identity.MailboxAccess
