import Proofs.SessionHydration.FileInventory

namespace SessionHydration.FileInventory.Order

open CanonicalOutput

/-- These are canonical publication positions, not tool IDs, block positions,
wall-clock observations or a new mutation journal. A completed publication
before the next admission establishes an edge even when requests differ.
Overlapping intervals do not establish an order between their mutations. -/
structure Window where
  tool : DocId
  admitted : Nat
  delivered : Nat
  deriving DecidableEq, Repr

def separated : List Window → Bool
  | [] | [_] => true
  | left :: right :: rest =>
      decide (left.delivered < right.admitted) && separated (right :: rest)

/-- This selects chronology only. Image continuity and inventory coverage must
still be established by their existing owners before producing a net diff. -/
def resolve (windows : List Window) : Option (List DocId) :=
  let ordered := windows.mergeSort (fun left right => left.admitted ≤ right.admitted)
  if (windows.map (·.tool)).Nodup &&
      windows.all (fun window => window.admitted < window.delivered) && separated ordered then
    some (ordered.map (·.tool))
  else none

theorem overlapping_adjacent_windows_are_unresolved (left right : Window)
    (rest : List Window) (h : right.admitted ≤ left.delivered) :
    separated (left :: right :: rest) = false := by
  simp [separated, Nat.not_lt.mpr h]

theorem empty_order : resolve [] = some [] := by simp [resolve, separated]

private def origin (input : ClosureInput) : Nat → MessageEnvelope → Option MessageEnvelope
  | 0, _ => none
  | fuel + 1, message =>
      match Hydration.checkedOrigin message.header with
      | .error _ => none
      | .ok none => some message
      | .ok (some id) =>
          match Hydration.lookupMessage input.messages input.deniedHeaders id with
          | .error _ => none
          | .ok original =>
              if Hydration.forkMetadataMatches message original then origin input fuel original
              else none

private def calls (tool : Tool) (message : MessageEnvelope) : Bool :=
  message.header.request == some tool.request && message.header.role == .assistant &&
    (match message.header.publication with | .requestExecution _ => true | _ => false) &&
    message.blocks.any (fun block => match block with
      | .toolCall id _ _ _ _ _ _ => id == tool.id
      | _ => false)

private def delivers (tool : Tool) (message : MessageEnvelope) : Bool :=
  message.header.request == some tool.request && message.header.role == .user &&
    message.header.publication == .toolDelivery tool.id &&
    (match message.blocks with
      | [.toolResult id _ _ _] => id == tool.id
      | _ => false)

private def uniquePosition (input : ClosureInput) (messages : List MessageEnvelope)
    (predicate : MessageEnvelope → Bool) : Option Nat :=
  match messages.filter (fun message =>
    (origin input (input.messages.length + 1) message).any predicate) with
  | [message] => some message.sequence
  | _ => none

/-- The snapshot and closure input are outputs of the existing authorized
inventory/hydration owners. Only selected target-session headers contribute
positions. Following a fork's origin validates that selected header; it never
adds an origin's later publications beyond the inherited cut. A missing or
ambiguous admission/delivery remains unresolved, including tools whose native
provenance does not supply a direct invocation-result pair. -/
def window (snapshot : Snapshot) (input : ClosureInput) (tool : Tool) : Option Window := do
  if input.request != snapshot.admission || !tool.accepted ||
      !(snapshot.tools.any (fun fact => fact.tool == tool)) ||
      !decide (HasTerminal.isTerminal tool.state) then none else pure ()
  let ids := (snapshot.documents.filter (fun key => key.collection == .agentMessage)).map (·.id)
  let messages ← ids.mapM fun id => (Hydration.lookupMessage input.messages input.deniedHeaders id).toOption
  let selected := messages.filter (fun message =>
    message.header.session == snapshot.admission.nativeSession)
  let admitted ← uniquePosition input selected (calls tool)
  let delivered ← uniquePosition input selected (delivers tool)
  if admitted < delivered then some ⟨tool.id, admitted, delivered⟩ else none

/-- All windows come from one authorized snapshot and its target-session
coordinate space. Selecting a resource never turns an unknown capture into
known coverage, and this result grants no retained-image access. -/
def comparison (snapshot : Snapshot) (input : ClosureInput)
    (node root path : String) : Option (List DocId) := do
  if input.request != snapshot.admission || [node, root, path].any String.isEmpty then
    none
  else pure ()
  let ids := ((snapshot.entries.filter (fun entry =>
    entry.scope.node == node && entry.change.root == root && entry.change.path == path)).map
      (·.tool)).dedup
  if ids.isEmpty then none else pure ()
  let windows ← ids.mapM fun id => do
    let tool ← match snapshot.tools.filter (fun fact => fact.tool.id == id) with
      | [fact] => some fact.tool
      | _ => none
    window snapshot input tool
  resolve windows

theorem foreign_closure_cannot_order (snapshot : Snapshot) (input : ClosureInput)
    (node root path : String) (h : input.request ≠ snapshot.admission) :
    comparison snapshot input node root path = none := by
  simp [comparison, h]

end SessionHydration.FileInventory.Order
