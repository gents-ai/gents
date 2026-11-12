import Mathlib.Data.List.Basic
import Lean

namespace ReadViews

open Lean

inductive Scope where
  | ownNode
  | callingAgent
  deriving DecidableEq, Repr, ToJson

def scopeMatches (scope : Scope) (node reader rowNode rowAgent : String) : Bool :=
  rowNode == node && match scope with
    | .ownNode => true
    | .callingAgent => rowAgent == reader

/-- Native ACP supplies readability for each document, including each joined
document. A view narrows that result; its node and agent fields never grant access. -/
def visible (scope : Scope) (node reader rowNode rowAgent : String)
    (readable : Bool) : Bool :=
  readable && scopeMatches scope node reader rowNode rowAgent

theorem visible_requires_readable (scope : Scope) (node reader rowNode rowAgent : String)
    (readable : Bool) (included : visible scope node reader rowNode rowAgent readable = true) :
    readable = true := by
  simp only [visible, Bool.and_eq_true] at included
  exact included.1

def rows {α : Type} (scope : Scope) (node : String) (reader : Option String)
    (nodeOf agentOf : α → String) (readable : String → α → Bool)
    (documents : List α) : List α :=
  match reader with
  | none => []
  | some actor => documents.filter fun row =>
      visible scope node actor (nodeOf row) (agentOf row) (readable actor row)

theorem rows_subset_readable {α : Type} (scope : Scope) (node : String)
    (reader : Option String) (nodeOf agentOf : α → String)
    (readable : String → α → Bool) (documents : List α) (row : α)
    (included : row ∈ rows scope node reader nodeOf agentOf readable documents) :
    ∃ actor, reader = some actor ∧ readable actor row = true := by
  cases reader with
  | none => simp [rows] at included
  | some actor =>
    have member := List.mem_filter.mp included
    exact ⟨actor, rfl,
      visible_requires_readable _ _ _ _ _ _ member.2⟩

theorem rows_require_explicit_actor {α : Type} (scope : Scope) (node : String)
    (nodeOf agentOf : α → String) (readable : String → α → Bool)
    (documents : List α) :
    rows scope node none nodeOf agentOf readable documents = [] := by
  rfl

theorem rows_stay_on_node {α : Type} (scope : Scope) (node actor : String)
    (nodeOf agentOf : α → String) (readable : String → α → Bool)
    (documents : List α) (row : α)
    (included : row ∈ rows scope node (some actor) nodeOf agentOf readable documents) :
    nodeOf row = node := by
  have member := (List.mem_filter.mp included).2
  simp only [visible, scopeMatches, Bool.and_eq_true, beq_iff_eq] at member
  exact member.2.1

theorem calling_agent_rows_stay_with_reader {α : Type} (node actor : String)
    (nodeOf agentOf : α → String) (readable : String → α → Bool)
    (documents : List α) (row : α)
    (included : row ∈ rows .callingAgent node (some actor) nodeOf agentOf readable documents) :
    agentOf row = actor := by
  have member := (List.mem_filter.mp included).2
  simp only [visible, scopeMatches, Bool.and_eq_true, beq_iff_eq] at member
  exact member.2.2

structure Decision where
  admitted : Bool
  included : Bool
  deriving DecidableEq, Repr, ToJson

def runner (scope : Scope) (actorPresent sameNode sameAgent readable : Bool) : Decision :=
  let actor := if actorPresent then some "reader" else none
  let rowNode := if sameNode then "node" else "other-node"
  let rowAgent := if sameAgent then "reader" else "other-reader"
  let result := rows scope "node" actor (fun (_ : Unit) => rowNode)
    (fun _ => rowAgent) (fun _ _ => readable) [()]
  ⟨actorPresent, !result.isEmpty⟩

inductive DocumentSelection where
  | any
  | matching
  | other
  | missing
  deriving DecidableEq, Repr, ToJson

structure CountDecision where
  admitted : Bool
  count : Nat
  deriving DecidableEq, Repr, ToJson

def counted (scope : Scope) (actorPresent sameNode sameAgent readable : Bool)
    (selection : DocumentSelection) : CountDecision :=
  let actor := if actorPresent then some "reader" else none
  let rowNode := if sameNode then "node" else "other-node"
  let rowAgent := if sameAgent then "reader" else "other-reader"
  let result := rows scope "node" actor (fun (_ : Unit) => rowNode)
    (fun _ => rowAgent) (fun _ _ => readable) [()]
  let selected := selection == .any || selection == .matching
  ⟨actorPresent, (result.filter fun _ => selected).length⟩

theorem missing_document_count_is_zero (scope : Scope)
    (actorPresent sameNode sameAgent readable : Bool) :
    (counted scope actorPresent sameNode sameAgent readable .missing).count = 0 := by
  simp [counted]

theorem count_requires_explicit_actor (scope : Scope)
    (sameNode sameAgent readable : Bool) (selection : DocumentSelection) :
    (counted scope false sameNode sameAgent readable selection).count = 0 := by
  simp [counted, rows]

theorem unreadable_document_count_is_zero (scope : Scope)
    (actorPresent sameNode sameAgent : Bool) (selection : DocumentSelection) :
    (counted scope actorPresent sameNode sameAgent false selection).count = 0 := by
  cases actorPresent <;> simp [counted, rows, visible]

end ReadViews
