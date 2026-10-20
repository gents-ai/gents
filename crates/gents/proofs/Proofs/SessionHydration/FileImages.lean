import Proofs.SessionHydration.State

namespace SessionHydration.FileImages

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

end SessionHydration.FileImages
