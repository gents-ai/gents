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
  let recovering := { groupedInvocation .running with
    journal := [{ index := 0, state := .executing }] }
  let completed := { groupedInvocation .running with
    journal := [{ index := 0, state := .resultDocsWritten }] }
  [ { name := "claim_preserves_captured_group",
      pre := groupedInvocation .pending,
      post := { groupedInvocation .pending with state := .claimed },
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
  ]

end Conformance
end Callback
