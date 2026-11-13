import Proofs.Shared.ToolPolicy.WriteInput

/-! A bounded write tool creates one document in its declared collection.
Runtime-filled fields are hidden from the caller's schema, refused when the
caller supplies them, and stamped from the running request at call time; an
optional source fill the trigger did not capture is omitted from the write. -/

namespace DatastoreWrite

open ToolPolicy.WriteInput

/-- Evaluation, optimization and pending chat submissions belong to their own
owners; a datastore tool may never write them, even when the collection
exists on the node. -/
def protectedCollections : List String :=
  ["EvalDefinition", "EvalRun", "EvalTrial", "EvalVerdict", "OptimizationJob",
   "PendingChatSubmission"]

inductive Fill where
  | caller
  | correlation
  | sourceField
  deriving DecidableEq, Repr

structure Field where
  name : String
  required : Bool
  fill : Fill
  deriving DecidableEq, Repr

/-- What the running request carries: whether it has a non-empty correlation,
and which trigger fields it captured. -/
structure Request where
  correlation : Bool
  captured : List String
  deriving DecidableEq, Repr

inductive Outcome where
  | written
  | refusedProtected
  | refusedRuntimeFilled
  | refusedInvalid
  | refusedUncorrelated
  deriving DecidableEq, Repr

/-- Which value a written field holds. -/
inductive Stored where
  | caller
  | correlation
  | sourceField
  | absent
  deriving DecidableEq, Repr

def wellFormed (collection : String) : Bool :=
  !protectedCollections.contains collection

def exposed (fields : List Field) : List String :=
  (fields.filter (·.fill == .caller)).map (·.name)

def admitted (fields : List Field) (supplied : List String) : Bool :=
  fields.all fun f =>
    admits .text true f.required (f.fill != .caller)
      (if supplied.contains f.name then some .text else none)

def call (collection : String) (fields : List Field) (supplied : List String)
    (request : Request) : Outcome :=
  if !wellFormed collection then .refusedProtected
  else if fields.any (fun f => f.fill != .caller && supplied.contains f.name) then
    .refusedRuntimeFilled
  else if !admitted fields supplied then .refusedInvalid
  else if fields.any (·.fill == .correlation) && !request.correlation then .refusedUncorrelated
  else .written

def stored (supplied : List String) (request : Request) (f : Field) : Stored :=
  match f.fill with
  | .caller => if supplied.contains f.name then .caller else .absent
  | .correlation => .correlation
  | .sourceField => if request.captured.contains f.name then .sourceField else .absent

/-- The written document, field by field in declaration order; the receipt
shows the same document and its id. -/
def written (collection : String) (fields : List Field) (supplied : List String)
    (request : Request) : Option (List Stored) :=
  if call collection fields supplied request = .written then
    some (fields.map (stored supplied request))
  else none

theorem rejects_a_persisted_protected_collection_at_execution (collection : String)
    (fields : List Field) (supplied : List String) (request : Request)
    (guarded : protectedCollections.contains collection = true) :
    wellFormed collection = false ∧ call collection fields supplied request = .refusedProtected := by
  refine ⟨by simp only [wellFormed, guarded, Bool.not_true], ?_⟩
  simp only [call, wellFormed, guarded, Bool.not_true, Bool.not_false, ↓reduceIte]

theorem runtime_fills_are_hidden_rejected_from_model_input_and_stamped_at_call_time
    (fields : List Field) (supplied : List String) (request : Request) (name : String) (f : Field)
    (shown : name ∈ exposed fields) (correlated : f.fill = .correlation) :
    (∃ g ∈ fields, g.fill = .caller ∧ g.name = name) ∧
      stored supplied request f = .correlation := by
  refine ⟨?_, by simp [stored, correlated]⟩
  simp only [exposed, List.mem_map, List.mem_filter, beq_iff_eq] at shown
  obtain ⟨g, ⟨mem, caller⟩, same⟩ := shown
  exact ⟨g, mem, caller, same⟩

theorem uncaptured_optional_source_fill_is_omitted_from_the_write (supplied : List String)
    (request : Request) (f : Field) (source : f.fill = .sourceField)
    (uncaptured : request.captured.contains f.name = false) :
    stored supplied request f = .absent := by
  simp only [stored, source, uncaptured, Bool.false_eq_true, ↓reduceIte]

end DatastoreWrite
