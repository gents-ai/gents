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

/-- The sweep fixture's hosting key is the native NAC owner. Its query bypasses
 document ACP (`Household.readObserved`), while every other actor's query
 follows the item's creator/recipient policy. This does not make that key a
 held target for the sweep. -/
def nativeSweepRead (hostingNode : Bool) (reader target : String) : Bool :=
  Household.readObserved hostingNode (sweepVisible reader target)

theorem hosting_node_read_does_not_establish_a_target :
    nativeSweepRead true "node" "agent" = true ∧ sweepVisible "node" "agent" = false := by decide

theorem runtime_node_is_not_mailbox_target :
    sweepVisible "node" "agent" = false ∧ sweepVisible "agent" "agent" = true := by decide

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

/-! ## Who an item is filed for -/

/-- How a request was admitted, read from its signed admission: by a person's
client, by an event or schedule trigger, as a runtime continuation of the
request before it (a retry, a Goal turn, a background-completion wake), or
otherwise (a delegated helper's request, a peer's). -/
inductive Admission where
  | client
  | automatedTrigger
  | localControl
  | other
  deriving DecidableEq, Repr

/-- Where a run started: the first admission that is not a continuation,
reading from the filing request back through its parents. -/
inductive LineageRoot where
  | automatedTrigger
  | other
  deriving DecidableEq, Repr

def lineageRoot : List Admission → LineageRoot
  | [] => .other
  | .localControl :: parents => lineageRoot parents
  | .automatedTrigger :: _ => .automatedTrigger
  | _ :: _ => .other

/-- Decision (gents#4191): who a filed item is for. `lineage` lists the filing
request's admission and then its parents'.

A request a person signed files for that person once its claim commits
(`claimedRecipient`). A run an event or schedule trigger admitted, and every
continuation in its lineage, files for the Node's owner: automation is Node
configuration, which the Node's owner administers, so the run acts for that
owner. The owner is the actor the store records as the Node document's
native owner, observed by the runtime reading its own Node; it is never read
from a field, the Task, the trigger's payload or the model, so a run cannot
name another person, and the owner administers every Agent on the Node, so it
could always message the run's Agent. An unobservable owner leaves no
recipient. Any other request without an admitted signer (a delegated helper, a
continuation of a person's turn) files for no one; its agent reports in its
reply instead. -/
def filingRecipient (claimedSigner : Option String) (lineage : List Admission)
    (nodeOwner : Option String) : Option String :=
  match claimedSigner with
  | some signer => some signer
  | none =>
    match lineageRoot lineage with
    | .automatedTrigger => nodeOwner
    | .other => none

theorem signed_request_files_for_its_signer (signer : String) (lineage : List Admission)
    (nodeOwner : Option String) :
    filingRecipient (some signer) lineage nodeOwner = some signer := rfl

theorem automation_files_only_for_the_node_owner (lineage : List Admission)
    (nodeOwner : Option String) (recipient : String)
    (h : filingRecipient none lineage nodeOwner = some recipient) :
    lineageRoot lineage = .automatedTrigger ∧ nodeOwner = some recipient := by
  unfold filingRecipient at h
  cases hroot : lineageRoot lineage <;> simp_all

theorem unattended_request_files_for_no_one (lineage : List Admission)
    (nodeOwner : Option String) (h : lineageRoot lineage = .other) :
    filingRecipient none lineage nodeOwner = none := by
  simp [filingRecipient, h]

theorem trigger_continuation_files_for_the_owner :
    filingRecipient none [.localControl, .localControl, .automatedTrigger] (some "owner") =
      some "owner" := rfl

theorem continuation_of_a_signed_turn_files_for_no_one :
    filingRecipient none [.localControl, .client] (some "owner") = none := rfl

theorem delegated_helper_files_for_no_one :
    filingRecipient none [.other] (some "owner") = none := rfl

theorem unobservable_owner_leaves_automation_without_recipient :
    filingRecipient none [.automatedTrigger] none = none := rfl

/-! ## What a client lists -/

/-- A stored item as a client reads it: its document and the requester it was
filed for, the DID its `recipient` relation names. -/
structure ItemRow where
  item : String
  requester : String
  deriving BEq, DecidableEq, Repr

/-- The items a client lists for `reader`: those filed for it, selected from
whatever its read observes.

Decision (gents#3747): a reader holding `bypass-dac` (the hosting node key, a
native admin such as the init User, `Household.readObserved`) observes every
item on the node, including items filed for another requester. Document ACP no
longer narrows that reader's rows, so the client selects by requester itself;
for a reader under document ACP the selection changes nothing. -/
def clientItems (reader : String) (observed : List ItemRow) : List ItemRow :=
  observed.filter (·.requester == reader)

def itemRows : List ItemRow := [⟨"item", "user"⟩, ⟨"foreign", "other"⟩]

/-- The rows `actor` observes from `itemRows`, each item carrying its
`recipient` relation as in `fixture`, with or without the bypass. -/
def observedRows (bypass : Bool) (actor : String) : List ItemRow :=
  itemRows.filter fun row =>
    Household.readObserved bypass
      (Household.allowedIn candidatePolicy 5
        { objects := [⟨"mailbox_items", row.item, "agent"⟩],
          relations := [⟨"mailbox_items", row.item, "recipient", .actor row.requester⟩],
          enrolled := [] }
        actor "mailbox_items" row.item "read")

theorem bypass_observes_foreign_items :
    observedRows true "user" = itemRows ∧ observedRows false "user" = [⟨"item", "user"⟩] := by
  native_decide

/-- The client lists the same items for its reader whether or not that reader
bypasses document ACP: only the ones filed for it. -/
theorem client_items_do_not_depend_on_bypass :
    ["user", "other", "stranger"].all (fun reader =>
      clientItems reader (observedRows true reader) ==
        clientItems reader (observedRows false reader)) = true ∧
    clientItems "user" (observedRows true "user") = [⟨"item", "user"⟩] := by
  native_decide

end Identity.MailboxAccess
