import Proofs.StreamingResponse.State

/-!
# Live target selection

`StreamingResponse.project` deliberately classifies one exact target.  This
module owns the missing observation step which chooses that target from the
current request generation.  It selects identity only; reconstruction,
closure validity, writer validity and liveness remain owned by `project`.
-/
namespace StreamingResponse

open CanonicalOutput

inductive TargetSelection where
  | absent
  | selected (target : Target)
  | conflicted
  deriving DecidableEq, Repr

private def providerKey? (request generation : Nat) (inferenceScopes : List Nat)
    (record : Segment) : Option (Nat × Nat × Nat × Coordinate × Writer) :=
  match record.coordinate.source, record.writer with
  | .provider scope turn attempt, .request writerGeneration =>
      if record.coordinate.request == request && writerGeneration == generation &&
          scope ∈ inferenceScopes then
        some (scope, turn, attempt, record.coordinate, record.writer)
      else none
  | _, _ => none

private def laterProvider
    (left right : Nat × Nat × Nat × Coordinate × Writer) :=
  if left.1 < right.1 ||
      (left.1 == right.1 && (left.2.1 < right.2.1 ||
        (left.2.1 == right.2.1 && left.2.2.1 < right.2.2.1)))
  then right else left

private def selectedProvider? (request generation : Nat) (inferenceScopes : List Nat)
    (records : List Segment) : Option (Coordinate × Writer) :=
  let candidates := (records.filterMap (providerKey? request generation inferenceScopes)).dedup
  (candidates.foldl (fun current candidate =>
    some (current.map (laterProvider · candidate) |>.getD candidate)) none).map
      (fun selected => (selected.2.2.2.1, selected.2.2.2.2))

private def headerReferencesCoordinate (records : List Segment)
    (coordinate : Coordinate) (writer : Writer) (message : MessageEnvelope) : Bool :=
  message.header.refs.any fun reference =>
    records.any fun record => record.id == reference.closeId &&
      record.coordinate == coordinate && record.writer == writer && record.close.isSome

/-- Select the latest provider attempt for the exact current request generation.
Header association is unique-or-conflict; no physical first/last winner exists. -/
def selectTarget (request generation : Nat) (inferenceScopes : List Nat) (records : List Segment)
    (messages : List MessageEnvelope) : TargetSelection :=
  match selectedProvider? request generation inferenceScopes records with
  | none => .absent
  | some (coordinate, writer) =>
      let ids := (messages.filterMap fun message =>
        if message.header.request == some request &&
            headerReferencesCoordinate records coordinate writer message then
          some message.header.id
        else none).dedup
      match ids with
      | [] => .selected ⟨coordinate, writer, none⟩
      | [id] => .selected ⟨coordinate, writer, some id⟩
      | _ => .conflicted

end StreamingResponse
