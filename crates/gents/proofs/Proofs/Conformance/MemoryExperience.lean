import Proofs.Tools.Memory.Experience
import Proofs.Conformance.Family

namespace Conformance.MemoryExperience

open Lean Tools.Memory.Experience

deriving instance ToJson for ProblemRow, AttemptRow, Score, OutcomeRow, Current, Provenance,
  Observation, FoldedAttempt, FoldedProblem, Experience, Index, MergeDecision, ScoreDecision,
  ScoreAdmission

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

def reader : Current := { agentDid := "did:key:self", executedModelName := some "model-a" }

def readerWithoutModel : Current := { reader with executedModelName := none }

def readerWithEmptyModel : Current := { reader with executedModelName := some "" }

def problem (key name : String) (createdAt : Nat) (carriedFrom : Option String := none) :
    ProblemRow :=
  { problemKey := key, name, carriedFrom, createdAt }

def attempt (key cited : String) (createdAt : Nat) (predicted : List String := [])
    (model : Option String := some "model-a") (carriedFrom : Option String := none) : AttemptRow :=
  { attemptKey := key, problemKey := cited, predicted, executedModelName := model, carriedFrom,
    createdAt }

def scored (passed total : Nat) : Option Score := some ⟨passed, total⟩

def outcome (key attemptKey : String) (createdAt : Nat) (score : Option Score)
    (fixed missed : List String := []) (model : Option String := some "model-a")
    (carriedFrom : Option String := none) : OutcomeRow :=
  { outcomeKey := key, attemptKey, fixed, missed, score, executedModelName := model, carriedFrom,
    createdAt }

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
    outcomes := [outcome "o-2" "a-2" 5 (scored 10 10) ["build"]]
    aliases := [⟨"p-4", "p-2"⟩] }

def provenances : RowsInput :=
  { problems := [problem "p-1" "Flaky login test" 1,
      problem "p-5" "Flaky login test on CI" 2 (some "did:key:source")]
    attempts := [attempt "a-1" "p-1" 3 ["login"] (some "model-b"),
      attempt "a-2" "p-5" 4 ["login"] (some "model-a") (some "did:key:source"),
      attempt "a-3" "p-5" 5 [] (some "model-b") (some "did:key:source"),
      attempt "a-4" "p-1" 6 [] (some "model-a") (some "did:key:self")] }

def unnamedModels : RowsInput :=
  { problems := [problem "p-1" "Flaky login test" 1]
    attempts := [attempt "a-1" "p-1" 2 ["login"] (some ""),
      attempt "a-2" "p-1" 3 [] none (some "did:key:source"),
      attempt "a-3" "p-1" 4 [] (some "model-a")]
    outcomes := [outcome "o-1" "a-1" 5 (scored 3 4) ["login"] [] (some "")] }

def observed : RowsInput :=
  { problems := [problem "p-1" "Flaky login test" 1, problem "p-2" "Slow build" 5,
      problem "p-3" "Cache misses after deploy" 8 (some "did:key:source"),
      problem "p-4" "Upload timeout" 11]
    attempts := [attempt "a-1" "p-1" 2 ["login", "logout"], attempt "a-2" "p-2" 6,
      attempt "a-3" "p-3" 9 [] (some "model-a") (some "did:key:source"),
      attempt "a-4" "p-4" 12 ["upload"]]
    outcomes := [outcome "o-1" "a-1" 3 (scored 8 10) ["login"],
      outcome "o-2" "a-1" 4 none [] ["logout"] (some "model-b"),
      outcome "o-3" "a-2" 7 (scored 7 10) [] [] (some "model-b"),
      outcome "o-4" "a-3" 10 (scored 5 5) [] [] (some "model-a") (some "did:key:source"),
      outcome "o-5" "a-4" 13 none [] [] none,
      outcome "o-6" "a-4" 14 none] }

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
            outcomes := [outcome "o-2" "a-1" 5 (scored 9 10) ["test_login"] ["test_logout"],
              outcome "o-1" "a-1" 3 (scored 8 10) ["test_login"]] } },
      { scenario := "fixed_then_missed", current := reader, rows :=
          { problems := [problem "p-flaky" "Flaky test timeout" 1]
            attempts := [{ attempt "a-1" "p-flaky" 2 ["test_login"] with accepted := false }]
            outcomes := [outcome "o-2" "a-1" 3 (scored 9 10) [] ["test_login"],
              outcome "o-1" "a-1" 3 (scored 10 10) ["test_login"]] } },
      { scenario := "alias_chain", current := reader, rows := chain },
      { scenario := "alias_cycle", current := reader, rows := cycle },
      { scenario := "unknown_keys", current := reader, rows :=
          { problems := [problem "p-1" "Flaky login test" 1]
            attempts := [attempt "a-1" "p-ghost" 2 ["login"], attempt "a-2" "p-1" 3]
            outcomes := [outcome "o-1" "a-missing" 4 (scored 1 2),
              outcome "o-2" "a-2" 5 (scored 2 2)] } },
      { scenario := "other_model_and_carried", current := reader, rows := provenances },
      { scenario := "reader_model_unknown", current := readerWithoutModel, rows := provenances },
      { scenario := "empty_and_absent_models", current := reader, rows := unnamedModels },
      { scenario := "reader_model_empty", current := readerWithEmptyModel, rows := unnamedModels },
      { scenario := "unscored_and_other_model_outcomes", current := reader, rows := observed },
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
      { scenario := "reader_model_unknown", current := readerWithoutModel, rows := provenances,
        ranks := [], budget := 1000 },
      { scenario := "empty_and_absent_models", current := reader, rows := unnamedModels,
        ranks := [], budget := 1000 },
      { scenario := "latest_observations", current := reader, rows := observed, ranks := [],
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
  deriving ToJson

def merge (scenario fromKey toKey : String) (aliases : List AliasRow := []) : MergeInput :=
  { scenario, problems := mergeProblems, aliases, fromProblemKey := fromKey,
    toProblemKey := toKey }

@[conformance_family]
def mergeAdmission : Family MergeInput MergeDecision where
  inputs :=
    [ merge "admitted" "p-1" "p-2",
      merge "admitted_reverse" "p-2" "p-1",
      merge "same_key" "p-1" "p-1",
      merge "unknown_from" "p-9" "p-1",
      merge "unknown_to" "p-1" "p-9",
      merge "already_merged" "p-1" "p-3" [⟨"p-1", "p-2"⟩],
      merge "closes_cycle" "p-1" "p-2" [⟨"p-2", "p-1"⟩],
      merge "closes_cycle_through_chain" "p-1" "p-3" [⟨"p-3", "p-2"⟩, ⟨"p-2", "p-1"⟩],
      merge "into_merged_key" "p-1" "p-2" [⟨"p-2", "p-3"⟩],
      merge "into_cycle" "p-1" "p-3" [⟨"p-2", "p-3"⟩, ⟨"p-3", "p-2"⟩] ]
  model := fun input => mergeAdmitted (input.problems.map (·.problemKey))
    (input.aliases.map fun row => (row.fromProblemKey, row.toProblemKey))
    input.fromProblemKey input.toProblemKey

/-- `existing` are the names the agent's problems were opened with; the
replay derives each one's key, and the root cause's, with production's key
derivation. -/
structure LandsOnInput where
  scenario : String
  agentDid : String
  existing : List String
  rootCause : String
  deriving ToJson

def opened : List String := ["Flaky login test", "Slow build", "Fehler in der Übersetzung"]

def lands (scenario rootCause : String) (existing : List String := opened) : LandsOnInput :=
  { scenario, agentDid := "did:key:self", existing, rootCause }

@[conformance_family]
def landsOn : Family LandsOnInput (Option String) where
  inputs :=
    [ lands "identical" "Flaky login test",
      lands "whitespace_and_ascii_case" "  flaky   LOGIN test\n",
      lands "other_text" "Flaky login tests",
      lands "upper_case" "SLOW BUILD",
      lands "no_break_space" "Slow\u00A0build",
      lands "em_space" "Slow\u2003 build",
      lands "non_ascii_kept" "fehler in der Übersetzung",
      lands "non_ascii_case_differs" "fehler in der übersetzung",
      lands "ascii_folded_around_non_ascii" "FEHLER IN DER Übersetzung",
      lands "empty_root_cause" " ",
      lands "no_problems" "Flaky login test" [] ]
  model := fun input => Tools.Memory.Experience.landsOn input.existing input.rootCause

structure ScoreInput where
  scenario : String
  passed : Option Int
  total : Option Int
  deriving ToJson

@[conformance_family]
def scoreAdmission : Family ScoreInput ScoreAdmission where
  inputs :=
    [ ⟨"scored", some 9, some 10⟩,
      ⟨"unscored", none, none⟩,
      ⟨"zero_of_zero", some 0, some 0⟩,
      ⟨"all_passed", some 10, some 10⟩,
      ⟨"passed_without_total", some 3, none⟩,
      ⟨"total_without_passed", none, some 3⟩,
      ⟨"negative_without_total", some (-1), none⟩,
      ⟨"negative_passed", some (-1), some 10⟩,
      ⟨"negative_total", some 0, some (-1)⟩,
      ⟨"negative_beside_exceeding", some 5, some (-2)⟩,
      ⟨"exceeds_total", some 11, some 10⟩,
      ⟨"largest", some 9223372036854775807, some 9223372036854775807⟩ ]
  model := fun input => scoreAdmitted input.passed input.total

end Conformance.MemoryExperience
