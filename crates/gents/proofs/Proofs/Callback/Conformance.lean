import Proofs.Callback.Properties

namespace Callback
namespace Conformance

open CallbackInvocation

structure CallbackCase where
  name : String
  invocationId : String
  ownerAgentDid : String
  state : InvocationState
  journal : List ActionJournalState
  resultEmitted : Bool
  legal : Bool
  deriving Repr

def numberedJournal (states : List ActionJournalState) : List ActionJournalEntry :=
  go 0 states
where
  go (i : Nat) : List ActionJournalState → List ActionJournalEntry
    | [] => []
    | s :: rest => { index := i, state := s } :: go (i + 1) rest

def CallbackCase.invocation (c : CallbackCase) : CallbackInvocation :=
  { invocationId := c.invocationId
  , ownerAgentDid := c.ownerAgentDid
  , state := c.state
  , journal := numberedJournal c.journal
  , resultEmitted := c.resultEmitted }

def caseLegalCorrect (c : CallbackCase) : Bool :=
  c.legal == invocationLegal c.invocation

def mkCase
    (name : String)
    (state : InvocationState)
    (journal : List ActionJournalState)
    (resultEmitted legal : Bool) : CallbackCase :=
  { name := name
  , invocationId := "inv-1"
  , ownerAgentDid := "dep-1"
  , state := state
  , journal := journal
  , resultEmitted := resultEmitted
  , legal := legal }

def callbackCases : List CallbackCase :=
  [ mkCase "happy_journal_prefix" .running
      [.resultDocsWritten, .executing] false true
  , mkCase "action_1_executing_while_0_not_result_docs_written_illegal" .running
      [.validated, .executing] false false
  , mkCase "result_emitted_while_running_illegal" .running
      [.resultDocsWritten] true false
  , mkCase "result_emitted_on_succeeded_with_complete_journal_legal" .succeeded
      [.resultDocsWritten, .resultDocsWritten] true true
  , mkCase "denied_empty_journal_legal" .denied [] false true
  , mkCase "denied_executing_journal_illegal" .denied [.executing] false false
  , mkCase "failed_after_result_docs_no_emit_legal" .failed
      [.resultDocsWritten] false true
  ]

theorem callbackCasesLegalCorrect :
    callbackCases.all caseLegalCorrect = true := by
  native_decide

def mkInv
    (invocationId ownerAgentDid : String)
    (state : InvocationState) : CallbackInvocation :=
  { invocationId := invocationId
  , ownerAgentDid := ownerAgentDid
  , state := state
  , journal := []
  , resultEmitted := false }

theorem claim_unique_two_active_same_key :
    ¬ ClaimUnique
      [mkInv "inv-1" "dep-1" .running, mkInv "inv-1" "dep-1" .claimed] := by
  native_decide

theorem claim_unique_different_ids :
    ClaimUnique
      [mkInv "inv-1" "dep-1" .running, mkInv "inv-2" "dep-1" .running] := by
  native_decide

/-- Proof-carrying lifecycle cases expose nonempty captured input and group
origin, so downstream conformance checks the real transition and journal. -/
structure TransitionCase where
  name : String
  pre : CallbackInvocation
  post : CallbackInvocation
  step : CallbackInvocation.Transition pre post

def groupedInvocation (state : InvocationState) : CallbackInvocation :=
  { invocationId := "inv-group", ownerAgentDid := "did:agent:a",
    input := "[{\"doc\":\"b\",\"value\":2},{\"doc\":\"a\",\"value\":1}]",
    originGroupKey := some ⟨"did:agent:a", .callbackBinding "summarize", "config-1", "run-1"⟩,
    state := state, journal := [], resultEmitted := false }

def transitionCases : List TransitionCase :=
  let interrupted := { groupedInvocation .failed with
    journal := [{ index := 0, state := .executing }], attempts := 1 }
  let recovering := { groupedInvocation .running with
    journal := [{ index := 0, state := .executing }] }
  let completed := { groupedInvocation .running with
    journal := [{ index := 0, state := .resultDocsWritten }] }
  [ { name := "claim_preserves_captured_group",
      pre := groupedInvocation .pending,
      post := { groupedInvocation .pending with state := .claimed, attempts := 1 },
      step := .claim rfl rfl }
  , { name := "run_preserves_captured_group",
      pre := groupedInvocation .claimed,
      post := { groupedInvocation .claimed with state := .running },
      step := .run rfl rfl }
  , { name := "failure_preserves_captured_group_and_journal",
      pre := recovering,
      post := { recovering with state := .failed, resultEmitted := false },
      step := .fail rfl rfl }
  , { name := "success_preserves_captured_group_and_emits_result",
      pre := completed,
      post := { completed with state := .succeeded, resultEmitted := true },
      step := .succeed rfl (by decide) rfl }
  , { name := "denied_claim_preserves_captured_group_without_actions",
      pre := groupedInvocation .claimed,
      post := { groupedInvocation .claimed with state := .denied, resultEmitted := false },
      step := .deny_claimed rfl rfl rfl }
  , { name := "denied_running_preserves_captured_group_without_actions",
      pre := groupedInvocation .running,
      post := { groupedInvocation .running with state := .denied, resultEmitted := false },
      step := .deny_running rfl rfl rfl }
  , { name := "retry_after_interrupted_attempt_starts_clean",
      pre := interrupted,
      post := { interrupted with state := .pending, journal := [], resultEmitted := false },
      step := .retry 3 (by decide) rfl }
  ]

/-- Every retry decision over a matrix of states, journals and budgets. The
expected answer is the model's own `retryAllowed`. -/
structure RetryCase where
  name : String
  state : InvocationState
  journal : List ActionJournalState
  attempts : Nat
  maxAttempts : Nat
  allowed : Bool
  deriving Repr

def retryCases : List RetryCase :=
  let states := [InvocationState.failed, .succeeded, .denied, .running]
  let journals : List (List ActionJournalState) :=
    [[], [.executing], [.validated], [.effectObserved], [.resultDocsWritten],
     [.resultDocsWritten, .executing]]
  states.flatMap fun state =>
    journals.flatMap fun journal =>
      [0, 1, 2, 3].flatMap fun attempts =>
        [1, 3].map fun maxAttempts =>
          let inv : CallbackInvocation :=
            { invocationId := "inv-1", ownerAgentDid := "dep-1", state := state,
              journal := numberedJournal journal, resultEmitted := false,
              attempts := attempts }
          { name := state.toDefraDB ++ ":" ++ String.intercalate ","
                (journal.map ActionJournalState.toDefraDB) ++ ":" ++ toString attempts
                ++ "/" ++ toString maxAttempts
            state := state, journal := journal, attempts := attempts,
            maxAttempts := maxAttempts, allowed := retryAllowed inv maxAttempts }

theorem retryCases_count : retryCases.length = 192 := by native_decide

end Conformance
end Callback
