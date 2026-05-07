import Proofs.Basic

/-!
# Shared Rust Conformance Contract Types

Small JSON-emission and finite-state helpers used by
`Proofs.Conformance.Contracts`.
-/

namespace Conformance.Contracts

structure TransitionPair where
  source : String
  target : String
  deriving DecidableEq, Repr

structure VocabularyContract where
  domain : String
  values : List String
  deriving Repr

structure StateMachineContract where
  domain : String
  states : List String
  stateCount : Nat
  terminalStates : List String
  nonterminalStates : List String
  actions : List String
  legalTransitions : List TransitionPair
  illegalTransitions : List TransitionPair
  deriving Repr

def jsonString (s : String) : String :=
  "\"" ++ s ++ "\""

def jsonArray (values : List String) : String :=
  "[" ++ String.intercalate "," values ++ "]"

def jsonStringArray (values : List String) : String :=
  jsonArray (values.map jsonString)

def jsonOptionalString : Option String → String
  | none => "null"
  | some value => jsonString value

def dedup {α : Type} [DecidableEq α] (values : List α) : List α :=
  values.foldl
    (fun seen value => if value ∈ seen then seen else seen ++ [value])
    []

def without {α : Type} [DecidableEq α] (values excluded : List α) : List α :=
  values.filter fun value => if value ∈ excluded then false else true

def allPairs (states : List String) : List TransitionPair :=
  states.flatMap fun source =>
    states.map fun target => { source := source, target := target }

def illegalPairs (states : List String) (legal : List TransitionPair) : List TransitionPair :=
  without (allPairs states) legal

def terminalNames {α : Type} [HasTerminal α]
    (states : List α)
    (name : α → String) : List String :=
  states.filterMap fun state =>
    if isTerminal state then some (name state) else none

def actionNames {α : Type} (actions : List (String × α)) : List String :=
  actions.map Prod.fst

def transitionPairsFromSamples {σ α : Type}
    (samples : List σ)
    (actions : List (String × α))
    (step : σ → α → Option σ)
    (stateName : σ → String) : List TransitionPair :=
  dedup <|
    samples.flatMap fun pre =>
      actions.filterMap fun action =>
        match step pre action.snd with
        | some post => some { source := stateName pre, target := stateName post }
        | none => none

def machineContract
    (domain : String)
    (states terminalStates actions : List String)
    (legalTransitions : List TransitionPair) : StateMachineContract :=
  let legalTransitions := dedup legalTransitions
  { domain := domain
  , states := states
  , stateCount := states.length
  , terminalStates := terminalStates
  , nonterminalStates := without states terminalStates
  , actions := actions
  , legalTransitions := legalTransitions
  , illegalTransitions := illegalPairs states legalTransitions
  }

def TransitionPair.toJson (pair : TransitionPair) : String :=
  "{"
    ++ "\"from\":" ++ jsonString pair.source ++ ","
    ++ "\"to\":" ++ jsonString pair.target
    ++ "}"

def VocabularyContract.toJson (contract : VocabularyContract) : String :=
  "{"
    ++ "\"domain\":" ++ jsonString contract.domain ++ ","
    ++ "\"values\":" ++ jsonStringArray contract.values
    ++ "}"

def StateMachineContract.toJson (contract : StateMachineContract) : String :=
  "{"
    ++ "\"domain\":" ++ jsonString contract.domain ++ ","
    ++ "\"states\":" ++ jsonStringArray contract.states ++ ","
    ++ "\"state_count\":" ++ toString contract.stateCount ++ ","
    ++ "\"terminal_states\":" ++ jsonStringArray contract.terminalStates ++ ","
    ++ "\"nonterminal_states\":" ++ jsonStringArray contract.nonterminalStates ++ ","
    ++ "\"actions\":" ++ jsonStringArray contract.actions ++ ","
    ++ "\"legal_transitions\":"
      ++ jsonArray (contract.legalTransitions.map TransitionPair.toJson) ++ ","
    ++ "\"illegal_transitions\":"
      ++ jsonArray (contract.illegalTransitions.map TransitionPair.toJson)
    ++ "}"

end Conformance.Contracts
