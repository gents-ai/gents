import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.Contracts.Machines.Process
import Proofs.Conformance.Contracts.Machines.Persistence
import Proofs.Conformance.Contracts.Machines.StorageObservation
import Proofs.Conformance.Contracts.Machines.RuntimeReconcile
import Proofs.Conformance.Contracts.Machines.SessionRecovery
import Proofs.Conformance.Contracts.Machines.InferenceCall
import Proofs.Conformance.Contracts.Machines.ToolCall
import Proofs.Conformance.Contracts.Machines.ManagedExec
import Proofs.Conformance.Contracts.Machines.Goal
import Proofs.Conformance.Contracts.Machines.Mailbox
import Proofs.Conformance.CompletionRetry.Contracts
import Proofs.Runtime.Eval
import Proofs.Conformance.Family

attribute [conformance_vocabulary] RequestState.toDefraDB
attribute [conformance_vocabulary] ExecutionOrigin.toDefraDB
attribute [conformance_vocabulary] ProcessState.toDefraDB
attribute [conformance_vocabulary] PersistenceState.FailurePolicy.toDefraDB
attribute [conformance_vocabulary] ReconcilePhase.toDefraDB
attribute [conformance_vocabulary] ToolExecution.ToolCallState.toDefraDB
attribute [conformance_vocabulary] ToolExecution.CancelCause.toDefraDB
attribute [conformance_vocabulary] ManagedExec.StopOutcome.toContract
attribute [conformance_vocabulary] ManagedExec.CancelReply.toContract
attribute [conformance_vocabulary] ToolExecution.FailureClass.toDefraDB
attribute [conformance_vocabulary] ToolExecution.AwaitMode.toDefraDB
attribute [conformance_vocabulary] Goals.Status.toDefraDB
attribute [conformance_vocabulary] Mailbox.Status.toDefraDB
attribute [conformance_vocabulary] Mailbox.Kind.toDefraDB
attribute [conformance_vocabulary] Mailbox.Handling.toDefraDB
attribute [conformance_vocabulary] Mailbox.SourceKind.toDefraDB
attribute [conformance_vocabulary] Eval.OutcomeKind.toDefraDB
attribute [conformance_vocabulary] Eval.ProviderReason.toDefraDB
attribute [conformance_vocabulary] Eval.EvidenceClass.toDefraDB

namespace Conformance.Contracts

def vocabularies : List VocabularyContract :=
  [ { domain := "RequestState", values := requestStateNames }
  , { domain := "PersistenceState", values := persistenceStateNames }
  , { domain := "PersistenceFailurePolicy", values :=
        [.failOpen, .failClosed].map PersistenceState.FailurePolicy.toDefraDB }
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
  , { domain := "ManagedExecState", values := managedExecStateNames }
  , { domain := "ToolRetryDisposition", values := toolRetryDispositionNames }
  , { domain := "GoalStatus", values := goalStatusNames }
  , { domain := "MailboxStatus", values := Mailbox.statusVocabulary }
  , { domain := "MailboxKind", values := Mailbox.kindVocabulary }
  , { domain := "MailboxHandling", values := Mailbox.handlingVocabulary }
  , { domain := "MailboxSourceKind", values := Mailbox.sourceKindVocabulary }
  , { domain := "EvalOutcomeKind", values := Eval.allKinds.map Eval.OutcomeKind.toDefraDB }
  , { domain := "EvalProviderReason", values := Eval.allReasons.map Eval.ProviderReason.toDefraDB }
  , { domain := "EvalEvidenceClass", values := Eval.allClasses.map Eval.EvidenceClass.toDefraDB }
  ]

def stateMachines : List StateMachineContract :=
  [ requestMachine
  , processMachine
  , persistenceMachine "Persistence.failClosed" .failClosed
  , persistenceMachine "Persistence.failOpen" .failOpen
  , storageObservationMachine "StorageObservation.failClosed" .failClosed
  , storageObservationMachine "StorageObservation.failOpen" .failOpen
  , runtimeReconcileMachine
  , sessionRecoveryMachine
  , inferenceCallMachine
  , toolCallMachine
  , managedExecMachine
  , goalMachine
  , mailboxMachine
  ]

end Conformance.Contracts
