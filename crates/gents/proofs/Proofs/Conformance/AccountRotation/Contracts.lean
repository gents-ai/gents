import Proofs.Shared.AccountRotation.Cases
import Proofs.Shared.AccountRotation.Pool
import Proofs.Conformance.Functions.Shared

namespace AccountRotation.Contracts

open Lean

deriving instance ToJson for Severity, Limit, Account, Policy, PoolChoice

inductive PoolOp where
  | choose
  | observe
  | reject
  | earliestReset
  | forgetAccount
  | forgetSession
  deriving DecidableEq, Repr, ToJson

structure PoolStep where
  op : PoolOp
  accounts : List String := []
  session : String := ""
  account : String := ""
  family : Family := .sonnet
  now : Nat := 100
  policy : Policy := {}
  reported : List Limit := []
  overage : Overage := .absent
  resetAt : Nat := 0
  deriving Repr, ToJson

structure PoolStepResult where
  choice : Option PoolChoice
  earliestReset : Option Nat
  cursor : Nat
  sessionCount : Nat
  recordedSessionAccount : Option String
  deriving DecidableEq, Repr, ToJson

structure PoolTraceInput where
  name : String
  sessionCap : Nat
  steps : List PoolStep
  deriving Repr, ToJson

def runPoolSteps : PoolState → List PoolStep → List PoolStepResult
  | _, [] => []
  | pool, step :: rest =>
    match step.op with
    | .choose =>
      let (pool', choice) :=
        pool.choose step.accounts step.session step.family step.now step.policy
      let result : PoolStepResult :=
        { choice
          earliestReset := none
          cursor := pool'.cursor
          sessionCount := pool'.sessions.length
          recordedSessionAccount := mapGet pool'.sessions step.session }
      result :: runPoolSteps pool' rest
    | .observe =>
      let pool' := pool.observe step.account step.family step.reported step.overage
      let result : PoolStepResult :=
        { choice := none
          earliestReset := none
          cursor := pool'.cursor
          sessionCount := pool'.sessions.length
          recordedSessionAccount := none }
      result :: runPoolSteps pool' rest
    | .reject =>
      let pool' := pool.reject step.account step.now step.resetAt
      let result : PoolStepResult :=
        { choice := none
          earliestReset := none
          cursor := pool'.cursor
          sessionCount := pool'.sessions.length
          recordedSessionAccount := none }
      result :: runPoolSteps pool' rest
    | .earliestReset =>
      let earliest := pool.earliestReset step.accounts step.family step.now step.policy
      let result : PoolStepResult :=
        { choice := none
          earliestReset := earliest
          cursor := pool.cursor
          sessionCount := pool.sessions.length
          recordedSessionAccount := none }
      result :: runPoolSteps pool rest
    | .forgetAccount =>
      let pool' := pool.forgetAccount step.account
      let result : PoolStepResult :=
        { choice := none
          earliestReset := none
          cursor := pool'.cursor
          sessionCount := pool'.sessions.length
          recordedSessionAccount := none }
      result :: runPoolSteps pool' rest
    | .forgetSession =>
      let pool' := pool.forgetSession step.session
      let result : PoolStepResult :=
        { choice := none
          earliestReset := none
          cursor := pool'.cursor
          sessionCount := pool'.sessions.length
          recordedSessionAccount := mapGet pool'.sessions step.session }
      result :: runPoolSteps pool' rest

private def chooseStep (accounts : List String) (session : String) (now : Nat := 100)
    (family : Family := .sonnet) (policy : Policy := {}) : PoolStep :=
  { op := .choose, accounts, session, now, family, policy }

private def observeStep (account : String) (reported : List Limit)
    (overage : Overage := .absent) (family : Family := .sonnet) : PoolStep :=
  { op := .observe, account, family, reported, overage }

private def rejectStep (account : String) (resetAt : Nat) (now : Nat := 100) : PoolStep :=
  { op := .reject, account, now, resetAt }

private def earliestResetStep (accounts : List String) (now : Nat := 100)
    (family : Family := .sonnet) (policy : Policy := {}) : PoolStep :=
  { op := .earliestReset, accounts, now, family, policy }

private def poolTraceInputs : List PoolTraceInput :=
  let three := ["a0", "a1", "a2"]
  let two := ["a0", "a1"]
  let tail := ["a1", "a2"]
  let allow : Policy := { allowOverage := true }
  [ { name := "round_robin_and_stickiness"
      sessionCap := 4
      steps :=
        [ chooseStep three "s1"
        , chooseStep three "s2"
        , chooseStep three "s3"
        , chooseStep three "s4"
        , chooseStep three "s1"
        , chooseStep three "s2" ] }
  , { name := "cursor_does_not_advance_when_known_session_switches"
      sessionCap := 4
      steps :=
        [ chooseStep three "s1"
        , observeStep "a0" [⟨.fiveHour, .warning, 900⟩]
        , chooseStep three "s1"
        , chooseStep three "s2" ] }
  , { name := "removed_account_advances_cursor_and_overwrites_at_cap_without_clearing"
      sessionCap := 2
      steps :=
        [ chooseStep three "s1"
        , chooseStep three "s2"
        , chooseStep tail "s1"
        , chooseStep tail "s2" ] }
  , { name := "cap_clear_loses_only_stickiness_not_limits"
      sessionCap := 2
      steps :=
        [ chooseStep three "s1"
        , chooseStep three "s2"
        , rejectStep "a0" 500
        , chooseStep three "s3"
        , chooseStep three "s1" ] }
  , { name := "zero_cap_never_retains_sessions"
      sessionCap := 0
      steps :=
        [ chooseStep two "s1"
        , chooseStep two "s1" ] }
  , { name := "overage_earliest_reset_respects_block_not_rejected_limit"
      sessionCap := 4
      steps :=
        [ observeStep "a0" [⟨.fiveHour, .rejected, 900⟩] .allowed
        , rejectStep "a0" 500 100
        , earliestResetStep ["a0"] 100 .sonnet {}
        , earliestResetStep ["a0"] 100 .sonnet allow
        , chooseStep ["a0"] "s1" 500 .sonnet allow ] }
  , { name := "forget_account_and_forget_session"
      sessionCap := 4
      steps :=
        [ chooseStep two "s1"
        , rejectStep "a0" 500
        , { op := .forgetAccount, account := "a0" }
        , { op := .forgetSession, session := "s1" }
        , chooseStep two "s1" ] }
  , { name := "all_exhausted_leaves_cursor_and_sessions_unchanged"
      sessionCap := 4
      steps :=
        [ chooseStep two "s1"
        , rejectStep "a0" 500
        , rejectStep "a1" 300
        , chooseStep two "s2"
        , earliestResetStep two 100
        , chooseStep two "s2" 300 ] } ]

@[conformance_family]
def poolTrace : Conformance.Family PoolTraceInput (List PoolStepResult) where
  inputs := poolTraceInputs
  model := fun i => runPoolSteps { sessionCap := i.sessionCap } i.steps

structure SelectInput where
  name : String
  policy : Policy
  now : Nat
  family : Family
  pool : List Account
  current : Option Nat
  cursor : Nat
  deriving ToJson

@[conformance_family]
def selection : Conformance.Family SelectInput (Option Nat) where
  inputs := selectCases.map fun c =>
    { name := c.name, policy := c.policy, now := caseNow, family := c.family, pool := c.pool
      current := c.current, cursor := c.cursor }
  model := fun i => select i.policy i.now i.family i.pool i.current i.cursor

structure Rejection where
  account : Nat
  resetAt : Nat
  deriving ToJson

structure AttemptInput where
  name : String
  policy : Policy
  now : Nat
  family : Family
  pool : List Account
  current : Option Nat
  cursor : Nat
  rejects : List Rejection
  deriving ToJson

@[conformance_family]
def attempts : Conformance.Family AttemptInput (List Nat) where
  inputs := attemptCases.map fun c =>
    { name := c.name, policy := c.policy, now := caseNow, family := c.family, pool := c.pool
      current := c.current, cursor := c.cursor
      rejects := c.rejects.map fun (account, resetAt) => { account, resetAt } }
  model := fun i =>
    attempt i.policy i.now i.family (rejectsOf (i.rejects.map fun r => (r.account, r.resetAt)))
      i.pool.length i.pool i.current i.cursor

structure ObserveInput where
  name : String
  family : Family
  account : Account
  reported : List Limit
  overage : Overage
  deriving ToJson

@[conformance_family]
def observation : Conformance.Family ObserveInput Account where
  inputs := observeCases.map fun c =>
    { name := c.name, family := c.family, account := c.account, reported := c.reported
      overage := c.overage }
  model := fun i => observe i.family i.reported i.overage i.account

end AccountRotation.Contracts
