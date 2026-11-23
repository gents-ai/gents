import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Automation.Callback.Conformance
import Proofs.Conformance.EventGroups
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

deriving instance Lean.ToJson for InvocationState, ActionJournalState, ActionJournalEntry

structure RecoveryInput where
  name : String
  journal : List ActionJournalState
  attempts : Nat
  maxAttempts : Nat
  deriving Lean.ToJson

def RecoveryInput.invocation (i : RecoveryInput) : CallbackInvocation :=
  { invocationId := "inv-1", ownerNodeDid := "dep-1", agentDid := "did:key:agent", state := .running,
    journal := Callback.Conformance.numberedJournal i.journal, resultEmitted := false,
    attempts := i.attempts }

def recoveryInputs : List RecoveryInput :=
  Callback.Conformance.recoveryCases.map fun c =>
    { name := c.name, journal := c.journal, attempts := c.attempts, maxAttempts := c.maxAttempts }

structure RecoveryOutput where
  postState : InvocationState
  postJournal : List ActionJournalEntry
  retryAllowedAfter : Bool
  deriving Lean.ToJson

/-- A running invocation after `recover_running` at startup, and whether
`retry_allowed` then permits another attempt. -/
@[conformance_family]
def callbackRecovery : Family RecoveryInput RecoveryOutput where
  inputs := recoveryInputs
  model := fun i =>
    let post := CallbackInvocation.recover i.invocation
    { postState := post.state, postJournal := post.journal
      retryAllowedAfter := CallbackInvocation.retryAllowed post i.maxAttempts }

structure DenialOutput where
  denyPostState : InvocationState
  denyPostJournal : List ActionJournalEntry
  retryAllowedAfterDeny : Bool
  deriving Lean.ToJson

/-- A running invocation after `apply_planner_deny`, and whether
`retry_allowed` then permits another attempt. -/
@[conformance_family]
def callbackDenial : Family RecoveryInput DenialOutput where
  inputs := recoveryInputs
  model := fun i =>
    let denied := CallbackInvocation.deny i.invocation
    { denyPostState := denied.state, denyPostJournal := denied.journal
      retryAllowedAfterDeny := CallbackInvocation.retryAllowed denied i.maxAttempts }

namespace CallbackJournal

open Lean

def journalOf (journal : List String) : List ActionJournalEntry :=
  Callback.Conformance.numberedJournal (journal.filterMap ActionJournalState.fromDefraDB?)

structure RetryInput where
  state : String
  journal : List String
  attempts : Nat
  maxAttempts : Nat
  deriving ToJson

structure JournalInput where
  agentDid : String
  ownerNodeDid : String
  journal : List String
  deriving ToJson

example : Callback.Conformance.callbackCases.all fun c => c.agentDid != c.ownerNodeDid := by native_decide

example : Callback.Conformance.retryCases.any (·.allowed) := by native_decide

end CallbackJournal

@[conformance_family]
def callbackRetry : Family CallbackJournal.RetryInput Bool where
  inputs := Callback.Conformance.retryCases.map fun c =>
    { state := c.state.toDefraDB, journal := c.journal.map ActionJournalState.toDefraDB
      attempts := c.attempts, maxAttempts := c.maxAttempts }
  model := fun i =>
    CallbackInvocation.retryAllowed
      { invocationId := "inv-1", ownerNodeDid := "dep-1", agentDid := "did:key:agent"
        state := (InvocationState.fromDefraDB? i.state).getD .pending
        journal := CallbackJournal.journalOf i.journal, resultEmitted := false
        attempts := i.attempts }
      i.maxAttempts

@[conformance_family]
def callbackJournal : Family CallbackJournal.JournalInput Bool where
  inputs := Callback.Conformance.callbackCases.map fun c =>
    { agentDid := c.agentDid, ownerNodeDid := c.ownerNodeDid
      journal := c.journal.map ActionJournalState.toDefraDB }
  model := fun i => CallbackInvocation.journalPrefixOk (CallbackJournal.journalOf i.journal)


structure CallbackJournalEntryWire where
  index : Nat
  state : String
  deriving Lean.ToJson

def CallbackJournalEntryWire.ofModel (e : ActionJournalEntry) : CallbackJournalEntryWire :=
  { index := e.index, state := e.state.toDefraDB }

/-- The step each transition witness takes, in `transitionCases` order. -/
def callbackTransitionSteps : List CallbackInvocation.Step :=
  [.claim, .run, .fail, .succeed, .denyClaimed, .denyRunning, .interrupt, .retry]

def callbackTransitionMaxAttempts : Nat := 3

example :
    (Callback.Conformance.transitionCases.zip callbackTransitionSteps).all (fun (c, s) =>
      CallbackInvocation.step? callbackTransitionMaxAttempts c.pre s == some c.post) &&
    callbackTransitionSteps.length == Callback.Conformance.transitionCases.length := by
  native_decide

/-- What the served engine does to one plugin invocation between checkpoints. -/
inductive SaturationStep where
  | claim
  | run
  | beginAction
  | writeResults
  | succeed
  deriving DecidableEq, Repr, Lean.ToJson

def SaturationStep.apply (inv : CallbackInvocation) : SaturationStep → CallbackInvocation
  | .claim => (CallbackInvocation.step? callbackTransitionMaxAttempts inv .claim).getD inv
  | .run => (CallbackInvocation.step? callbackTransitionMaxAttempts inv .run).getD inv
  | .beginAction => CallbackInvocation.beginAction inv
  | .writeResults => CallbackInvocation.writeResults inv
  | .succeed => (CallbackInvocation.step? callbackTransitionMaxAttempts inv .succeed).getD inv

structure SaturationTrace where
  name : String
  checkpoints : List (List SaturationStep)
  deriving Lean.ToJson

structure SaturationCheckpoint where
  state : String
  attempts : Nat
  journal : List CallbackJournalEntryWire
  resultEmitted : Bool
  deriving Lean.ToJson

def SaturationCheckpoint.ofModel (inv : CallbackInvocation) : SaturationCheckpoint :=
  { state := inv.state.toDefraDB, attempts := inv.attempts
  , journal := inv.journal.map CallbackJournalEntryWire.ofModel, resultEmitted := inv.resultEmitted }

/-- The saturation scenario observes a plugin invocation left pending beyond
capacity, the invocations held inside their single HTTP action, and every
invocation after its action's results are written. -/
@[conformance_family]
def callbackSaturationTrace : Family SaturationTrace (List SaturationCheckpoint) where
  inputs :=
    [ { name := "saturated_plugin_recovers_without_new_arrivals"
      , checkpoints := [[], [.claim, .run, .beginAction], [.writeResults, .succeed]] } ]
  model := fun i =>
    let (_, observed) := i.checkpoints.foldl
      (fun (inv, observed) steps =>
        let next := steps.foldl SaturationStep.apply inv
        (next, observed ++ [SaturationCheckpoint.ofModel next]))
      (Callback.Conformance.groupedInvocation .pending, [])
    observed

end Conformance.Contracts
