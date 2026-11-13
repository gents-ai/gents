import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.Family

def RequestState.terminal (state : RequestState) : Bool :=
  decide (isTerminal state)

def ToolExecution.ToolCallState.terminal (state : ToolExecution.ToolCallState) : Bool :=
  decide (isTerminal state)

attribute [conformance_function] RequestState.terminal ToolExecution.ToolCallState.terminal

namespace Conformance.Contracts

deriving instance Lean.ToJson for TransitionPair, NamedTransition

end Conformance.Contracts

namespace Conformance.StateMachines

open Lean Conformance.Contracts

structure CatalogInput where
  domain : String
  deriving ToJson

structure Catalog where
  states : List String
  stateCount : Nat
  terminalStates : List String
  nonterminalStates : List String
  actions : List String
  legalTransitions : List TransitionPair
  illegalTransitions : List TransitionPair
  namedTransitions : List NamedTransition
  deriving ToJson

def Catalog.ofContract (machine : StateMachineContract) : Catalog :=
  { states := machine.states
  , stateCount := machine.stateCount
  , terminalStates := machine.terminalStates
  , nonterminalStates := machine.nonterminalStates
  , actions := machine.actions
  , legalTransitions := machine.legalTransitions
  , illegalTransitions := machine.illegalTransitions
  , namedTransitions := machine.namedTransitions }

def machineOf (domain : String) : Option StateMachineContract :=
  stateMachines.find? (·.domain == domain)

/-- model-production-gap: production has no transition function over these
catalogs; each owner of requests, processes, persistence, storage observations,
reconciles, recoveries, inference calls, tool calls, managed processes, goals and
mailbox items writes its transitions inline, so only the terminal partitions are
replayed (`request_state_terminal`, `tool_execution_tool_call_state_terminal`,
`mailbox_status_terminal`). -/
@[conformance_family]
def catalogUnreached : Family CatalogInput (Option Catalog) where
  inputs := stateMachines.map fun machine => { domain := machine.domain }
  model := fun i => (machineOf i.domain).map Catalog.ofContract

def wellFormed (machine : StateMachineContract) : Bool :=
  let pairs := machine.legalTransitions ++ machine.illegalTransitions
  machine.stateCount == machine.states.length
    && decide machine.states.Nodup
    && decide machine.actions.Nodup
    && decide machine.legalTransitions.Nodup
    && decide machine.illegalTransitions.Nodup
    && pairs.length == machine.stateCount * machine.stateCount
    && machine.legalTransitions.all (fun pair => !machine.illegalTransitions.contains pair)
    && pairs.all (fun pair => machine.states.contains pair.source && machine.states.contains pair.target)

def legal (machine : StateMachineContract) (source target : String) : Bool :=
  machine.legalTransitions.contains { source, target }
    && !machine.illegalTransitions.contains { source, target }

def illegal (machine : StateMachineContract) (source target : String) : Bool :=
  machine.illegalTransitions.contains { source, target }
    && !machine.legalTransitions.contains { source, target }

example : stateMachines.all wellFormed = true := by native_decide

example : (stateMachines.map (·.domain)).Nodup := by native_decide

example :
    (legal requestMachine "pending" "claimed"
      && legal requestMachine "claimed" "processing"
      && legal requestMachine "processing" "processing"
      && legal requestMachine "claimed" "failed"
      && legal requestMachine "pending" "interrupted"
      && legal requestMachine "processing" "interrupted"
      && illegal requestMachine "interrupted" "failed") = true := by native_decide

example :
    (legal managedExecMachine "pendingSpawn" "running"
      && legal managedExecMachine "running" "killSignaled"
      && legal managedExecMachine "killSignaled" "killed") = true := by native_decide

example :
    (legal inferenceCallMachine "queued" "running"
      && legal inferenceCallMachine "queued" "cancelled"
      && legal inferenceCallMachine "running" "completed"
      && legal inferenceCallMachine "running" "failed"
      && legal inferenceCallMachine "running" "cancelled"
      && illegal inferenceCallMachine "queued" "completed"
      && illegal inferenceCallMachine "completed" "running") = true := by native_decide

example :
    (legal runtimeReconcileMachine "applying" "idle"
      && legal runtimeReconcileMachine "idle" "debouncing") = true := by native_decide

example :
    let closed := persistenceMachine "Persistence.failClosed" .failClosed
    let opened := persistenceMachine "Persistence.failOpen" .failOpen
    (legal closed "committing" "uncommitted" && legal opened "committing" "lost") = true := by
  native_decide

example :
    let closed := storageObservationMachine "StorageObservation.failClosed" .failClosed
    let opened := storageObservationMachine "StorageObservation.failOpen" .failOpen
    (legal closed "noMutation" "inFlight"
      && legal closed "inFlight" "successAcknowledged"
      && legal closed "inFlight" "mutationFailed"
      && legal closed "mutationFailed" "noMutation"
      && legal opened "mutationFailed" "lostAcknowledged"
      && legal closed "successAcknowledged" "staleObserved"
      && legal closed "successAcknowledged" "readVisible"
      && legal closed "staleObserved" "readVisible"
      && illegal closed "mutationFailed" "lostAcknowledged"
      && illegal opened "mutationFailed" "noMutation") = true := by native_decide

example :
    (legal sessionRecoveryMachine "failed" "pending"
      && illegal sessionRecoveryMachine "dead" "pending"
      && illegal sessionRecoveryMachine "superseded" "pending"
      && illegal sessionRecoveryMachine "interrupted" "pending") = true := by native_decide

example :
    mailboxMachine.terminalStates
      = (mailboxStatuses.filter Mailbox.Status.terminal).map Mailbox.Status.toDefraDB := by
  native_decide

example : mailboxMachine.nonterminalStates = ["open"] := by native_decide

example : mailboxMachine.legalTransitions.all (·.source == "open") = true := by native_decide

end Conformance.StateMachines
