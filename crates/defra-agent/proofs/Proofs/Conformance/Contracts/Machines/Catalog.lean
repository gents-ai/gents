import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.Contracts.Machines.Process
import Proofs.Conformance.Contracts.Machines.Persistence
import Proofs.Conformance.Contracts.Machines.StorageObservation
import Proofs.Conformance.Contracts.Machines.RuntimeReconcile
import Proofs.Conformance.Contracts.Machines.PairingReconcile
import Proofs.Conformance.Contracts.Machines.SessionRecovery
import Proofs.Conformance.Contracts.Machines.InferenceCall
import Proofs.Conformance.Contracts.Machines.ToolCall
import Proofs.Conformance.Contracts.Machines.ManagedExec
import Proofs.Conformance.Contracts.Machines.Subagent
import Proofs.CompletionRetry.Contracts

/-!
# Conformance Machine Catalog

Aggregate vocabulary and state-machine lists consumed by the JSON snapshot.
-/

namespace Conformance.Contracts

def vocabularies : List VocabularyContract :=
  [ { domain := "RequestState", values := requestStateNames }
  , { domain := "ExecutionOrigin", values :=
        [.interactive, .scheduled].map ExecutionOrigin.toDefraDB }
  , { domain := "ProcessState", values := processStateNames }
  , { domain := "PersistenceState", values := persistenceStateNames }
  , { domain := "PersistenceFailurePolicy", values :=
        [.failOpen, .failClosed].map PersistenceState.FailurePolicy.toDefraDB }
  , { domain := "ReconcilePhase", values := runtimeReconcileStateNames }
  , { domain := "StorageObservation", values := storageObservationStateNames }
  , { domain := "SessionRecoveryLatestRequestState"
    , values := requestStateNames
    }
  , { domain := "InferenceCallState", values := inferenceCallStateNames }
  , { domain := "InferenceCallTerminalReason", values :=
        [ .cancelled
        , .backendGone
        , .queueFull
        , .streamDroppedBeforeTerminalResponse
        ].map InferenceCallTerminalReason.toDefraDB
    }
  , { domain := "CompletionRetryFailureClass"
    , values := CompletionRetry.Contracts.failureClassVocabulary
    }
  , { domain := "ToolCallState", values := toolCallStateNames }
  , { domain := "CancelCause", values := toolCallCancelCauseNames }
  , { domain := "ManagedExecState", values := managedExecStateNames }
  , { domain := "ToolFailureClass", values := failureClassNames }
  , { domain := "ToolRetryDisposition", values := toolRetryDispositionNames }
  , { domain := "AwaitMode"
    , values := Subagent.AwaitMode.all.map Subagent.AwaitMode.toDefraDB
    }
  , { domain := "CancelPolicy"
    , values := Subagent.CancelPolicy.all.map Subagent.CancelPolicy.toDefraDB
    }
  , { domain := "ChildTerminal"
    , values := ["failed", "dead", "interrupted", "superseded"]
    }
  ]

def stateMachines : List StateMachineContract :=
  [ requestMachine
  , processMachine
  , persistenceMachine "Persistence.failClosed" .failClosed
  , persistenceMachine "Persistence.failOpen" .failOpen
  , storageObservationMachine "StorageObservation.failClosed" .failClosed
  , storageObservationMachine "StorageObservation.failOpen" .failOpen
  , runtimeReconcileMachine
  , pairingReconcileMachine
  , sessionRecoveryMachine
  , inferenceCallMachine
  , toolCallMachine
  , managedExecMachine
  , awaitModeMachine
  , cancelPolicyMachine
  , childTerminalMachine
  ]

end Conformance.Contracts
