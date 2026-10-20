import Proofs.SessionHydration.State
import Proofs.Identity.Permission

namespace SessionHydration.FileImages

inductive StoragePermission where
  | read
  deriving DecidableEq, Repr

/-- DefraDB retains the signed creator as each replica's document owner.
A verified delivery receipt authorizes only its named requester to retain a
local read grant. The creator keeps its existing rights; unrelated actors gain
none. Revocation prevents future delivery, not reads of bytes already delivered. -/
def storageReadGrants (creator requester : Identity.DID) (granted : Bool) :
    Identity.GrantStore StoragePermission :=
  ⟨fun actor _ => (!creator.isEmpty && actor == creator) ||
    (granted && !requester.isEmpty && actor == requester)⟩

def storedImageReadable (creator requester : Identity.DID) (granted : Bool)
    (actor : Option Identity.DID) : Bool :=
  actor.any (fun did => (storageReadGrants creator requester granted).granted did .read)

def authorizesReadGrant (source requester signer localActor storedCreator : Identity.DID)
    (verified : Bool) : Bool :=
  verified && !source.isEmpty && !requester.isEmpty && signer == source &&
    storedCreator == source && localActor == requester

theorem anonymous_cannot_read_stored_image (creator requester : Identity.DID) (granted : Bool) :
    storedImageReadable creator requester granted none = false := by rfl

theorem foreign_actor_cannot_read_stored_image (creator requester actor : Identity.DID)
    (granted : Bool) (hc : actor ≠ creator) (hr : actor ≠ requester) :
    storedImageReadable creator requester granted (some actor) = false := by
  simp [storedImageReadable, storageReadGrants, hc, hr]

theorem creator_can_read (creator requester : Identity.DID) (granted : Bool)
    (h : creator.isEmpty = false) :
    storedImageReadable creator requester granted (some creator) = true := by
  simp [storedImageReadable, storageReadGrants, h]

theorem requester_can_read_after_grant (creator requester : Identity.DID)
    (h : requester.isEmpty = false) :
    storedImageReadable creator requester true (some requester) = true := by
  simp [storedImageReadable, storageReadGrants, h]

theorem unverified_receipt_cannot_grant (source requester signer localActor storedCreator : Identity.DID) :
    authorizesReadGrant source requester signer localActor storedCreator false = false := by
  simp [authorizesReadGrant]

structure Demand where
  admission : Request
  requestDocId : String
  toolDocId : String
  deriving DecidableEq, Repr

structure Scope where
  requester : String
  node : String
  session : String
  deriving DecidableEq, Repr

structure Tool where
  docId : String
  requestDocId : String
  scope : Scope
  accepted : Bool
  terminal : Bool
  deriving DecidableEq, Repr

/-- The immutable observation names the accepted physical tool. Native readers
validate its bytes through the file-image owner before transport. Images are
not part of the ordinary transcript closure. -/
structure Image where
  docId : String
  toolDocId : String
  requestDocId : String
  scope : Scope
  deriving DecidableEq, Repr

inductive Selection where
  | rejected
  | missing
  | selected (document : String)
  deriving DecidableEq, Repr

def scopeFor (demand : Demand) : Scope :=
  ⟨demand.admission.requester, demand.admission.node, demand.admission.session⟩

def wellFormed (demand : Demand) : Bool :=
  [demand.admission.key, demand.admission.peer, demand.admission.requester,
   demand.admission.node, demand.admission.session, demand.requestDocId,
   demand.toolDocId].all (fun value => !value.isEmpty)

def toolMatches (demand : Demand) (tool : Tool) : Bool :=
  tool.accepted && tool.terminal && tool.docId == demand.toolDocId &&
  tool.requestDocId == demand.requestDocId && tool.scope == scopeFor demand

def imageMatches (demand : Demand) (image : Image) : Bool :=
  !image.docId.isEmpty && image.toolDocId == demand.toolDocId &&
  image.requestDocId == demand.requestDocId && image.scope == scopeFor demand

/-- Pairing, current membership and session ownership retain their existing
admission owner. A successful empty selection requires one accepted terminal
tool. Missing rows and duplicate observations never widen access. -/
def select (demand : Demand) (catalog : Catalog) (tools : List Tool)
    (images : List Image) : Selection :=
  if !wellFormed demand || !decide (admits catalog demand.admission) then .rejected
  else match tools.filter (fun tool => tool.docId == demand.toolDocId) with
  | [tool] =>
      if !toolMatches demand tool then .rejected
      else match images.filter (fun image => image.toolDocId == demand.toolDocId) with
      | [] => .missing
      | [image] => if imageMatches demand image then .selected image.docId else .rejected
      | _ => .rejected
  | _ => .rejected

theorem unauthorized_is_rejected (demand : Demand) (catalog : Catalog)
    (tools : List Tool) (images : List Image)
    (h : ¬ admits catalog demand.admission) :
    select demand catalog tools images = .rejected := by
  simp [select, h]

theorem malformed_is_rejected (demand : Demand) (catalog : Catalog)
    (tools : List Tool) (images : List Image) (h : wellFormed demand = false) :
    select demand catalog tools images = .rejected := by
  simp [select, h]

theorem unaccepted_tool_is_rejected (demand : Demand) (catalog : Catalog)
    (tool : Tool) (images : List Image) (h : tool.accepted = false) :
    select demand catalog [tool] images = .rejected := by
  by_cases hid : tool.docId = demand.toolDocId <;> simp [select, hid, toolMatches, h]

theorem unfinished_tool_is_rejected (demand : Demand) (catalog : Catalog)
    (tool : Tool) (images : List Image) (h : tool.terminal = false) :
    select demand catalog [tool] images = .rejected := by
  by_cases hid : tool.docId = demand.toolDocId <;> simp [select, hid, toolMatches, h]

theorem duplicate_tool_is_rejected (demand : Demand) (catalog : Catalog)
    (tool : Tool) (images : List Image) :
    select demand catalog [tool, tool] images = .rejected := by
  by_cases hid : tool.docId = demand.toolDocId <;> simp [select, hid]

theorem mismatched_image_is_rejected (demand : Demand) (catalog : Catalog)
    (tool : Tool) (image : Image)
    (ht : tool.docId = demand.toolDocId) (hi : image.toolDocId = demand.toolDocId)
    (hm : imageMatches demand image = false) :
    select demand catalog [tool] [image] = .rejected := by
  simp [select, ht, hi, hm]

def deliveryTarget (selection : Selection) (authorized : Bool) : Option String :=
  if !authorized then none
  else match selection with
  | .selected document => some document
  | .missing | .rejected => none

/-- A terminal receipt is durable only after its write commits. Unknown
transport completion leaves the demand pending; authorization is checked again
before acknowledging a delivered observation. Missing images have no payload
but still require current authorization and a committed receipt. -/
def complete (selection : Selection) (beforeDelivery afterDelivery : Bool)
    (delivery : DeliveryResult) (write : TerminalWriteResult) : Option Selection :=
  if write != .committed then none
  else if !beforeDelivery || selection == .rejected then some .rejected
  else match delivery with
  | .indeterminate => none
  | .confirmed => if !afterDelivery then some .rejected else some selection

theorem revoked_has_no_delivery_target (selection : Selection) :
    deliveryTarget selection false = none := by simp [deliveryTarget]

theorem failed_receipt_write_stays_pending (selection : Selection)
    (beforeDelivery afterDelivery : Bool) (delivery : DeliveryResult) :
    complete selection beforeDelivery afterDelivery delivery .failed = none := by
  simp [complete]

theorem unconfirmed_delivery_cannot_be_acknowledged (selection : Selection)
    (afterDelivery : Bool) (write : TerminalWriteResult)
    (h : selection ≠ .rejected) :
    complete selection true afterDelivery .indeterminate write = none := by
  simp [complete, h]

theorem revoked_after_delivery_cannot_serve (selection : Selection)
    (beforeDelivery : Bool) (write : TerminalWriteResult) (document : String) :
    complete selection beforeDelivery false .confirmed write ≠ some (.selected document) := by
  simp [complete]

/-- The prior outcome is read from the source's immutable, signed receipt for
this exact demand, never from the client's mutable status projection. A new
explicit retry uses another demand key and therefore has no prior outcome. -/
def recordOutcome (prior : Option Selection) (candidate : Selection)
    (write : TerminalWriteResult) : Option Selection :=
  match prior with
  | some outcome => some outcome
  | none => if write == .committed then some candidate else none

def attemptDeliveryTarget (prior : Option Selection) (selection : Selection)
    (authorized : Bool) : Option String :=
  match prior with
  | some _ => none
  | none => deliveryTarget selection authorized

theorem recorded_outcome_is_preserved (prior candidate : Selection)
    (write : TerminalWriteResult) :
    recordOutcome (some prior) candidate write = some prior := by rfl

theorem recorded_attempt_does_not_redeliver (prior candidate : Selection)
    (authorized : Bool) :
    attemptDeliveryTarget (some prior) candidate authorized = none := by rfl

theorem fresh_failed_attempt_has_no_outcome (candidate : Selection) :
    recordOutcome none candidate .failed = none := by rfl

theorem refusal_cannot_be_replaced_by_restored_authorization (document : String) :
    recordOutcome (some .rejected) (.selected document) .committed = some .rejected := by rfl

end SessionHydration.FileImages
