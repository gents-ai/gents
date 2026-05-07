import Proofs.Request
import Proofs.Process
import Proofs.Conformance.Boundaries
import Proofs.Conformance.ContractCases.Types

/-!
# Lifecycle Transition Case Partitions

Generated Request and Process source/target pairs for Rust conformance tests.
Each finite state square is classified as legal, ordinary illegal, or
product-unreachable. The current product-unreachable Request pairs are exactly
the reserved `inputRequired` vocabulary surface.
-/

namespace Conformance.ContractCases

inductive LifecycleTransitionClassification where
  | legal
  | illegal
  | productUnreachable
  deriving DecidableEq, Repr

namespace LifecycleTransitionClassification

def toContract : LifecycleTransitionClassification → String
  | .legal => "legal"
  | .illegal => "illegal"
  | .productUnreachable => "productUnreachable"

end LifecycleTransitionClassification

def lifecycleTransitionCaseName (domain source target : String) : String :=
  domain ++ ":" ++ source ++ "->" ++ target

/-- Return the action that witnesses a source/target pair in the supplied finite
    samples. Current Request and Process lifecycle contracts intentionally keep
    a one-action-per-pair surface; if a future model adds aliases for the same
    pair, update the emitted case shape instead of relying on list order. -/
def actionForPairFromSamples {σ α : Type}
    (samples : List σ)
    (actions : List (String × α))
    (step : σ → α → Option σ)
    (stateName : σ → String)
    (source target : String) : Option String :=
  let candidates :=
    samples.flatMap fun pre =>
      actions.filterMap fun action =>
        match step pre action.snd with
        | some post =>
            if stateName pre = source ∧ stateName post = target then
              some action.fst
            else
              none
        | none => none
  candidates.head?

def requestTransitionStates : List RequestState :=
  [ .pending, .claimed, .processing, .inputRequired, .completed
  , .failed, .superseded, .dead, .interrupted ]

def requestTransitionActions : List (String × RequestContext.Action) :=
  [ ("claim", .claim)
  , ("dedupLose", .dedupLose)
  , ("beginInference", .beginInference)
  , ("advance", .advance)
  , ("finish", .finish)
  , ("fail", .fail)
  , ("failBeforeStream", .failBeforeStream)
  , ("expire", .expire)
  , ("interruptBeforeClaim", .interruptBeforeClaim)
  , ("interruptClaimed", .interruptClaimed)
  , ("interruptProcessing", .interruptProcessing)
  ]

def requestTransitionContext
    (state : RequestState)
    (admission : AdmissionState)
    (hasInterrupt : Bool := false)
    (validUntil : Option Time := none)
    (currentTime : Time := 0) : RequestContext :=
  { state := state
  , origin := .interactive
  , backend := contractBackend
  , admission := admission
  , deadline := 10
  , claimTime := 0
  , currentTime := currentTime
  , retryCount := 0
  , maxRetries := 3
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .uncommitted
  , interruptRequestedAt := if hasInterrupt then some currentTime else none
  , validUntil := validUntil
  }

/-- Finite witnesses for action preconditions. Every legal Request action must
    have at least one sample satisfying its guards here, otherwise its pair is
    classified as denied and the Rust generated-case harness will report drift. -/
def requestTransitionSamples : List RequestContext :=
  [ requestTransitionContext .pending .released
  , requestTransitionContext .pending .released true
  , requestTransitionContext .pending .released false (some 0) 1
  , requestTransitionContext .claimed .waiting
  , requestTransitionContext .claimed .acquired
  , requestTransitionContext .claimed .waiting true
  , requestTransitionContext .claimed .acquired true
  , requestTransitionContext .processing .executing
  , requestTransitionContext .processing .executing true
  , requestTransitionContext .inputRequired .executing
  , requestTransitionContext .completed .released
  , requestTransitionContext .failed .released
  , requestTransitionContext .superseded .released
  , requestTransitionContext .dead .released
  , requestTransitionContext .interrupted .released
  ]

def requestTransitionAction? (source target : String) : Option String :=
  actionForPairFromSamples
    requestTransitionSamples
    requestTransitionActions
    RequestContext.step?
    (fun ctx => ctx.state.toDefraDB)
    source
    target

def requestTransitionClassification
    (source target : RequestState)
    (action : Option String) : LifecycleTransitionClassification :=
  match action with
  | some _ => .legal
  | none =>
      -- Current product policy has exactly one reserved request state. Adding
      -- another reserved persisted state must update this classifier and the
      -- Rust reserved-state assertion together.
      if source = .inputRequired ∨ target = .inputRequired then
        .productUnreachable
      else
        .illegal

def requestTransitionCase (source target : RequestState) : LifecycleTransitionCase :=
  let sourceName := source.toDefraDB
  let targetName := target.toDefraDB
  let action := requestTransitionAction? sourceName targetName
  let classification := requestTransitionClassification source target action
  { name := lifecycleTransitionCaseName "Request" sourceName targetName
  , domain := "Request"
  , fromState := sourceName
  , toState := targetName
  , classification := classification.toContract
  , action := action
  , boundary :=
      match classification with
      | .productUnreachable =>
          some Conformance.Contracts.boundaryRequestInputRequiredReservedId
      | _ => none
  }

def requestTransitionCases : List LifecycleTransitionCase :=
  requestTransitionStates.flatMap fun source =>
    requestTransitionStates.map fun target =>
      requestTransitionCase source target

def processTransitionStates : List ProcessState :=
  [ .uninitialized, .recovering, .ready, .shuttingDown, .shutdown ]

def processTransitionActions : List (String × ProcessState.Action) :=
  [ ("startupRecover", .startupRecover { hasStuckRequests := true, activeRequestCount := 1 })
  , ("startupClean", .startupClean { hasStuckRequests := false, activeRequestCount := 0 })
  , ("recoveryComplete", .recoveryComplete)
  , ("beginShutdown", .beginShutdown)
  , ("finishShutdown", .finishShutdown 0)
  ]

def processTransitionAction? (source target : String) : Option String :=
  actionForPairFromSamples
    processTransitionStates
    processTransitionActions
    ProcessState.step?
    ProcessState.toDefraDB
    source
    target

def processTransitionClassification
    (action : Option String) : LifecycleTransitionClassification :=
  match action with
  | some _ => .legal
  | none => .illegal

def processTransitionCase (source target : ProcessState) : LifecycleTransitionCase :=
  let sourceName := source.toDefraDB
  let targetName := target.toDefraDB
  let action := processTransitionAction? sourceName targetName
  let classification := processTransitionClassification action
  { name := lifecycleTransitionCaseName "Process" sourceName targetName
  , domain := "Process"
  , fromState := sourceName
  , toState := targetName
  , classification := classification.toContract
  , action := action
  , boundary := none
  }

def processTransitionCases : List LifecycleTransitionCase :=
  processTransitionStates.flatMap fun source =>
    processTransitionStates.map fun target =>
      processTransitionCase source target

end Conformance.ContractCases
