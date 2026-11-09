import Proofs.FileInventory.Source
import Proofs.Identity.Permission

namespace FileInventory.FileImages

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

/-- A document holds only the before and after versions for one physical tool
observation. A DefraDB document grant exposes every version, so independently
authorized observations must never share a content document. -/
structure Content where
  docId : String
  toolDocId : String
  scope : Scope
  deriving DecidableEq, Repr

/-- The immutable observation names the accepted physical tool and the file
documents whose versions hold its before and after bytes. Native readers
validate those bytes through the file-image owner before transport. Images are
not part of the ordinary transcript closure. -/
structure Image where
  docId : String
  toolDocId : String
  requestDocId : String
  scope : Scope
  contents : List Content
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

/-- Delivery carries the observation and every file document it references;
the read grant covers the same set. -/
def deliveryDocuments (image : Image) : List String :=
  image.docId :: image.contents.map (·.docId)

end FileInventory.FileImages
