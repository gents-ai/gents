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
