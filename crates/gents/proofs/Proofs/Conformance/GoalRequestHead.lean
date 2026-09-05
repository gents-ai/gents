import Proofs.GoalAutomation.RequestHead
import Proofs.Conformance.ContractTypes

namespace Conformance.GoalRequestHeadContracts

open GoalAutomation.RequestHead Conformance.Contracts

structure Case where
  name : String
  rows : List Row
  expected : Option Row
  deriving DecidableEq, Repr

def scope : Scope := ⟨1, 1, 1⟩
def root : Row := ⟨10, 100, 1, 1, none, none, none, 0, true, true⟩
def child : Row := ⟨20, 200, 1, 1, some 1, some 10, some 100, 1, true, true⟩
def grandchild : Row := ⟨30, 300, 1, 1, some 1, some 20, some 200, 2, true, true⟩
def unrelated : Row := ⟨40, 400, 1, 1, none, none, none, 0, true, true⟩

/-- Explicit query-order inputs and expected heads. The graph/task names label
actual lexical order witnesses; numeric IDs are abstract physical identities. -/
def cases : List Case :=
  [ ⟨"same_second_graph_parent", [root, child], some child⟩
  , ⟨"legacy_source_omission_keeps_physical_edge", [root, child], some child⟩
  , ⟨"same_second_task_parent_chain", [root, grandchild, child], some grandchild⟩
  , ⟨"reverse_lexical_order", [child, root], some child⟩
  , ⟨"unrelated_latest_keeps_priority", [unrelated, root, child], some unrelated⟩
  , ⟨"foreign_edge_cannot_suppress", [root, {child with owner := 2}], some root⟩
  , ⟨"arbitrary_signed_parent_link_cannot_suppress", [root, {child with goal := none}], some root⟩
  , ⟨"other_goal_link_cannot_suppress", [root, {child with goal := some 2}], some root⟩
  , ⟨"missing_parent_does_not_suppress_unrelated_head", [root, {child with parentDoc := some 99}], some root⟩
  , ⟨"mismatched_physical_pair_does_not_suppress", [root, {child with parentRequest := some 999}], some root⟩
  , ⟨"invalid_signature_does_not_suppress", [root, {child with receiptValid := false}], some root⟩
  , ⟨"wrong_deterministic_identity_does_not_suppress", [root, {child with deterministicIdentity := false}], some root⟩
  , ⟨"branch_leaves_keep_canonical_order", [root, child, {grandchild with parentDoc := some 10, parentRequest := some 100}], some child⟩
  , ⟨"new_goal_epoch_may_reset_sequence", [root, {child with sequence := 5}, {grandchild with sequence := 1}], some {grandchild with sequence := 1}⟩
  , ⟨"empty_session", [], none⟩
  ]

-- Abstract relation only: mutually deterministic hash identities are not a
-- realizable signed-row fixture. This is not emitted as consumer coverage.
example : select scope [{child with parentDoc := some 30, parentRequest := some 300}, grandchild] = none := by decide

theorem cases_replay_explicit_expectations : ∀ c ∈ cases, select scope c.rows = c.expected := by decide


private def boolJson (value : Bool) : String := if value then "true" else "false"
private def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def rowJson (row : Row) : String :=
  "{\"doc\":" ++ toString row.doc ++ ",\"request\":" ++ toString row.request ++
  ",\"owner\":" ++ toString row.owner ++ ",\"session\":" ++ toString row.session ++
  ",\"goal\":" ++ optionalNatJson row.goal ++ ",\"parent_doc\":" ++ optionalNatJson row.parentDoc ++
  ",\"parent_request\":" ++ optionalNatJson row.parentRequest ++ ",\"sequence\":" ++ toString row.sequence ++
  ",\"receipt_valid\":" ++ boolJson row.receiptValid ++
  ",\"deterministic_identity\":" ++ boolJson row.deterministicIdentity ++ "}"

def caseJson (testCase : Case) : String :=
  "{\"name\":" ++ jsonString testCase.name ++
  ",\"scope\":{\"owner\":" ++ toString scope.owner ++
  ",\"session\":" ++ toString scope.session ++ ",\"goal\":" ++ toString scope.goal ++ "}" ++
  ",\"rows\":" ++ jsonArray (testCase.rows.map rowJson) ++
  ",\"expected\":" ++ (match testCase.expected with | none => "null" | some row => rowJson row) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

theorem cases_count : cases.length = 15 := by decide

end Conformance.GoalRequestHeadContracts
