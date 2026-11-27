import Proofs.Tools.Memory.Experience
import Proofs.Conformance.Family

namespace Conformance.MemoryExperience

open Lean Tools.Memory.Experience

deriving instance ToJson for ProblemRow, AttemptRow, OutcomeRow, Current, Provenance, Score,
  FoldedAttempt, FoldedProblem, Experience, Index, MergeDecision

structure AliasRow where
  fromProblemKey : String
  toProblemKey : String
  deriving ToJson

structure RowsInput where
  problems : List ProblemRow := []
  attempts : List AttemptRow := []
  outcomes : List OutcomeRow := []
  aliases : List AliasRow := []
  deriving ToJson

def RowsInput.rows (input : RowsInput) : Rows :=
  { problems := input.problems
    attempts := input.attempts
    outcomes := input.outcomes
    aliases := input.aliases.map fun row => (row.fromProblemKey, row.toProblemKey) }

def reader : Current := { agentDid := "did:key:self", executedModelName := "model-a" }

def problem (key name : String) (createdAt : Nat) (carriedFrom : Option String := none) :
    ProblemRow :=
  { problemKey := key, name, carriedFrom, createdAt }

def attempt (key cited : String) (createdAt : Nat) (predicted : List String := [])
    (model : String := "model-a") (carriedFrom : Option String := none) : AttemptRow :=
  { attemptKey := key, problemKey := cited, predicted, executedModelName := model, carriedFrom,
    createdAt }

def outcome (key attemptKey : String) (createdAt passed total : Nat)
    (fixed missed : List String := []) : OutcomeRow :=
  { outcomeKey := key, attemptKey, fixed, missed, passed, total, createdAt }

def chain : RowsInput :=
  { problems := [problem "p-a" "Cache stale after deploy" 1, problem "p-b" "Stale cache entries" 2,
      problem "p-c" "Cache invalidation missing" 3]
    attempts := [attempt "a-1" "p-a" 4 ["cache_test"], attempt "a-2" "p-b" 5,
      attempt "a-3" "p-c" 6]
    aliases := [⟨"p-a", "p-b"⟩, ⟨"p-b", "p-c"⟩] }

def cycle : RowsInput :=
  { problems := [problem "k-m" "Timeout on upload" 1, problem "k-q" "Upload retries exhausted" 2,
      problem "k-c" "Upload chunk size too large" 3]
    attempts := [attempt "a-1" "k-m" 4, attempt "a-2" "k-q" 5, attempt "a-3" "k-c" 6]
    aliases := [⟨"k-m", "k-q"⟩, ⟨"k-q", "k-c"⟩, ⟨"k-c", "k-q"⟩] }

def index : RowsInput :=
  { problems := [problem "p-1" "Flaky login test" 1, problem "p-2" "Slow build" 3,
      problem "p-3" "Memory leak in worker" 6, problem "p-4" "Build takes too long" 7]
    attempts := [attempt "a-1" "p-1" 2 ["login"], attempt "a-2" "p-2" 4 ["build"]]
    outcomes := [outcome "o-2" "a-2" 5 10 10 ["build"]]
    aliases := [⟨"p-4", "p-2"⟩] }

def provenances : RowsInput :=
  { problems := [problem "p-1" "Flaky login test" 1,
      problem "p-5" "Flaky login test on CI" 2 (some "did:key:source")]
    attempts := [attempt "a-1" "p-1" 3 ["login"] "model-b",
      attempt "a-2" "p-5" 4 ["login"] "model-a" (some "did:key:source"),
      attempt "a-3" "p-5" 5 [] "model-b" (some "did:key:source"),
      attempt "a-4" "p-1" 6 [] "model-a" (some "did:key:self")] }

def longRootCause : String :=
  "The integration suite fails when the payment sandbox rate-limits parallel workers, " ++
    "because every worker opens its own session and retries without backoff on HTTP 429"

structure FoldInput where
  scenario : String
  current : Current
  rows : RowsInput
  deriving ToJson

@[conformance_family]
def fold : Family FoldInput Experience where
  inputs :=
    [ { scenario := "empty", current := reader, rows := {} },
      { scenario := "attempt_without_outcome", current := reader, rows :=
          { problems := [problem "p-flaky" "Flaky test timeout" 1]
            attempts := [attempt "a-1" "p-flaky" 2 ["test_login", "test_login", "test_logout"]] } },
      { scenario := "multiple_outcomes", current := reader, rows :=
          { problems := [problem "p-flaky" "Flaky test timeout" 1]
            attempts := [attempt "a-1" "p-flaky" 2 ["test_login", "test_logout", "test_signup"]]
            outcomes := [outcome "o-2" "a-1" 5 9 10 ["test_login"] ["test_logout"],
              outcome "o-1" "a-1" 3 8 10 ["test_login"]] } },
      { scenario := "fixed_then_missed", current := reader, rows :=
          { problems := [problem "p-flaky" "Flaky test timeout" 1]
            attempts := [{ attempt "a-1" "p-flaky" 2 ["test_login"] with accepted := false }]
            outcomes := [outcome "o-2" "a-1" 3 9 10 [] ["test_login"],
              outcome "o-1" "a-1" 3 10 10 ["test_login"]] } },
      { scenario := "alias_chain", current := reader, rows := chain },
      { scenario := "alias_cycle", current := reader, rows := cycle },
      { scenario := "unknown_keys", current := reader, rows :=
          { problems := [problem "p-1" "Flaky login test" 1]
            attempts := [attempt "a-1" "p-ghost" 2 ["login"], attempt "a-2" "p-1" 3]
            outcomes := [outcome "o-1" "a-missing" 4 1 2, outcome "o-2" "a-2" 5 2 2] } },
      { scenario := "other_model_and_carried", current := reader, rows := provenances },
      { scenario := "merged_index", current := reader, rows := index } ]
  model := fun input => Tools.Memory.Experience.fold input.current input.rows.rows

structure RecallInput where
  scenario : String
  current : Current
  rows : RowsInput
  ranks : List String
  budget : Nat
  deriving ToJson

@[conformance_family]
def recall : Family RecallInput Index where
  inputs :=
    [ { scenario := "everything_fits", current := reader, rows := index, ranks := [],
        budget := 1000 },
      { scenario := "retrieved_first", current := reader, rows := index, ranks := ["p-3"],
        budget := 1000 },
      { scenario := "retrieved_by_merged_key", current := reader, rows := index,
        ranks := ["p-4", "p-3"], budget := 1000 },
      { scenario := "truncated", current := reader, rows := index, ranks := [], budget := 120 },
      { scenario := "two_lines_fit_exactly", current := reader, rows := index, ranks := [],
        budget := 146 },
      { scenario := "zero_budget", current := reader, rows := index, ranks := [], budget := 0 },
      { scenario := "long_root_cause", current := reader, rows :=
          { problems := [problem "p-long" longRootCause 1]
            attempts := [attempt "a-1" "p-long" 2 ["payments"]] }, ranks := [], budget := 1000 },
      { scenario := "provenance_flags", current := reader, rows := provenances, ranks := [],
        budget := 1000 },
      { scenario := "alias_cycle", current := reader, rows := cycle, ranks := ["k-m"],
        budget := 1000 },
      { scenario := "empty", current := reader, rows := {}, ranks := ["p-1"], budget := 1000 } ]
  model := fun input =>
    Tools.Memory.Experience.recall (Tools.Memory.Experience.fold input.current input.rows.rows)
      input.ranks input.budget

def mergeProblems : List ProblemRow :=
  [problem "p-1" "Flaky login test" 1, problem "p-2" "Slow build" 2,
    problem "p-3" "Fehler in der Übersetzung" 3]

structure MergeInput where
  scenario : String
  problems : List ProblemRow
  aliases : List AliasRow
  fromProblemKey : String
  toProblemKey : String
  rootCause : String
  deriving ToJson

structure MergeOutput where
  decision : MergeDecision
  landsOn : Option String
  deriving ToJson

def merge (scenario fromKey toKey rootCause : String) (aliases : List AliasRow := []) :
    MergeInput :=
  { scenario, problems := mergeProblems, aliases, fromProblemKey := fromKey,
    toProblemKey := toKey, rootCause }

@[conformance_family]
def mergeAdmission : Family MergeInput MergeOutput where
  inputs :=
    [ merge "admitted" "p-1" "p-2" "  flaky   LOGIN test\n",
      merge "same_key" "p-1" "p-1" "Flaky login test",
      merge "unknown_from" "p-9" "p-1" "Flaky login tests",
      merge "unknown_to" "p-1" "p-9" "SLOW BUILD",
      merge "already_merged" "p-1" "p-3" "Slow\u00A0build" [⟨"p-1", "p-2"⟩],
      merge "closes_cycle" "p-1" "p-2" "Slow\u2003 build" [⟨"p-2", "p-1"⟩],
      merge "closes_cycle_through_chain" "p-1" "p-3" "fehler in der Übersetzung"
        [⟨"p-3", "p-2"⟩, ⟨"p-2", "p-1"⟩],
      merge "into_merged_key" "p-1" "p-2" "fehler in der übersetzung" [⟨"p-2", "p-3"⟩],
      merge "into_cycle" "p-1" "p-3" "FEHLER IN DER Übersetzung" [⟨"p-2", "p-3"⟩, ⟨"p-3", "p-2"⟩],
      merge "empty_root_cause" "p-2" "p-1" " " ]
  model := fun input =>
    { decision := mergeAdmitted (input.problems.map (·.problemKey))
        (input.aliases.map fun row => (row.fromProblemKey, row.toProblemKey))
        input.fromProblemKey input.toProblemKey
      landsOn := landsOn input.problems input.rootCause }

end Conformance.MemoryExperience
