import Proofs.Runtime.RuntimeReconcile
import Lean
import Mathlib.Data.Finset.Sort
import Proofs.Conformance.ContractCases.Types
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Automation

namespace Conformance.ContractCases

def runtimeResolvedA : ResolvedSnapshot :=
  { defaultAgent := 10, runnable := {10}, unavailable := ∅
  , dependenciesSatisfied := {10} }

def runtimeResolvedB : ResolvedSnapshot :=
  { defaultAgent := 20, runnable := {20}, unavailable := {10}
  , dependenciesSatisfied := {20} }

def runtimeResolvedMissingDependency : ResolvedSnapshot :=
  { defaultAgent := 20, runnable := {20}, unavailable := {10}
  , dependenciesSatisfied := ∅ }

def runtimeBoot : RuntimeState :=
  RuntimeState.bootState runtimeResolvedA

def runtimeApplyingChanged : RuntimeState :=
  { runtimeBoot with phase := .applying, pendingResolved := some runtimeResolvedB }

def runtimePublishedBeforeRouter : RuntimeState :=
  { runtimeBoot with
    lastResolved := runtimeResolvedB
  , active := runtimeResolvedB.activate 2
  , routerObservedGeneration := 1
  , readyGenerations := {1, 2}
  , liveGenerations := {1, 2}
  }

def runtimeRouterObserved : RuntimeState :=
  { runtimePublishedBeforeRouter with routerObservedGeneration := 2 }

def runtimeWithInFlight : RuntimeState :=
  { runtimeRouterObserved with
    accepted := {500}
  , inFlight := {500}
  , requestGeneration := Function.update runtimeRouterObserved.requestGeneration 500 2
  , requestSession := Function.update runtimeRouterObserved.requestSession 500 100
  , requestAgent := Function.update runtimeRouterObserved.requestAgent 500 20
  , sessionAgent := Function.update runtimeRouterObserved.sessionAgent 100 (some 20)
  }

def runtimeCaseFromStep
    (name actionName : String)
    (pre : RuntimeState)
    (action : RuntimeState.Action)
    (trackedRequestId : RequestId := 0)
    (trackedSessionId : SessionId := 0) : RuntimeReconcileCase :=
  let requested := match action with
    | .acceptRequest _ _ _ agent => some agent
    | _ => none
  match RuntimeState.step? pre action with
  | some post =>
      { requestedAgent := requested
      , preDefaultAgent := pre.active.defaultAgent
      , preSessionAgent := pre.sessionAgent trackedSessionId
      , preRunnable := pre.effectiveDispatchers.sort (· ≤ ·)
      , name := name
      , action := actionName
      , legal := true
      , prePhase := pre.phase.toDefraDB
      , postPhase := post.phase.toDefraDB
      , preActiveGeneration := pre.active.generation
      , postActiveGeneration := post.active.generation
      , preRouterGeneration := pre.routerObservedGeneration
      , postRouterGeneration := post.routerObservedGeneration
      , preReadyGenerationCount := pre.readyGenerations.card
      , postReadyGenerationCount := post.readyGenerations.card
      , preLiveGenerationCount := pre.liveGenerations.card
      , postLiveGenerationCount := post.liveGenerations.card
      , preInFlightCount := pre.inFlight.card
      , postInFlightCount := post.inFlight.card
      , trackedRequestId := trackedRequestId
      , trackedSessionId := trackedSessionId
      , trackedRequestGeneration := post.requestGeneration trackedRequestId
      , trackedRequestSession := post.requestSession trackedRequestId
      , trackedRequestAgent := post.requestAgent trackedRequestId
      , trackedSessionAgent :=
          match post.sessionAgent trackedSessionId with
          | some agentDid => agentDid
          | none => 0
      }
  | none =>
      { requestedAgent := requested
      , preDefaultAgent := pre.active.defaultAgent
      , preSessionAgent := pre.sessionAgent trackedSessionId
      , preRunnable := pre.effectiveDispatchers.sort (· ≤ ·)
      , name := name
      , action := actionName
      , legal := false
      , prePhase := pre.phase.toDefraDB
      , postPhase := ""
      , preActiveGeneration := pre.active.generation
      , postActiveGeneration := 0
      , preRouterGeneration := pre.routerObservedGeneration
      , postRouterGeneration := 0
      , preReadyGenerationCount := pre.readyGenerations.card
      , postReadyGenerationCount := 0
      , preLiveGenerationCount := pre.liveGenerations.card
      , postLiveGenerationCount := 0
      , preInFlightCount := pre.inFlight.card
      , postInFlightCount := 0
      , trackedRequestId := trackedRequestId
      , trackedSessionId := trackedSessionId
      , trackedRequestGeneration := 0
      , trackedRequestSession := 0
      , trackedRequestAgent := 0
      , trackedSessionAgent := 0
      }

def runtimeTwoAgents : RuntimeState := RuntimeState.bootState
  { defaultAgent := 10, runnable := {10, 20}, unavailable := ∅,
    dependenciesSatisfied := {10, 20} }

def runtimeExplicitSelectionCases : List RuntimeReconcileCase :=
  [ runtimeCaseFromStep "default-A-explicit-B-binds-B" "acceptRequest" runtimeTwoAgents
      (.acceptRequest .ready 100 500 20) 500 100
  , runtimeCaseFromStep "existing-A-requested-B-rejected" "acceptRequest"
      { runtimeTwoAgents with sessionAgent := fun _ => some 10 }
      (.acceptRequest .ready 100 500 20) 500 100
  , runtimeCaseFromStep "existing-A-requested-A-accepted" "acceptRequest"
      { runtimeTwoAgents with sessionAgent := fun _ => some 10 }
      (.acceptRequest .ready 100 500 10) 500 100
  , runtimeCaseFromStep "unknown-selection-never-falls-back" "acceptRequest" runtimeTwoAgents
      (.acceptRequest .ready 100 500 999) 500 100
  , runtimeCaseFromStep "unobserved-generation-rejects-explicit-request" "acceptRequest"
      runtimePublishedBeforeRouter (.acceptRequest .ready 100 500 20) 500 100
  , runtimeCaseFromStep "demoted-selection-never-falls-back" "acceptRequest"
      { runtimeTwoAgents with startupDemoted := {20} }
      (.acceptRequest .ready 100 500 20) 500 100 ]

theorem explicit_selection_cases_pinned : runtimeExplicitSelectionCases.map
    (fun c => (c.legal, c.trackedRequestAgent, c.trackedSessionAgent)) =
    [(true, 20, 20), (false, 0, 0), (true, 10, 10), (false, 0, 0),
     (false, 0, 0), (false, 0, 0)] := by native_decide

def runtimeReconcileCases : List RuntimeReconcileCase :=
  runtimeExplicitSelectionCases ++ [ runtimeCaseFromStep
      "publish_changed_snapshot"
      "publish"
      runtimeApplyingChanged
      (.publish runtimeResolvedB)
  , runtimeCaseFromStep
      "router_observe_published_generation"
      "routerObserve"
      runtimePublishedBeforeRouter
      (.routerObserve .ready)
  , runtimeCaseFromStep
      "accept_request_after_router_observe"
      "acceptRequest"
      runtimeRouterObserved
      (.acceptRequest .ready 100 500 20)
      500
      100
  , runtimeCaseFromStep
      "finish_request_releases_generation"
      "finishRequest"
      runtimeWithInFlight
      (.finishRequest 500)
      500
      100
  , runtimeCaseFromStep
      "replayed_request_is_not_accepted_twice"
      "acceptRequest"
      { runtimeWithInFlight with inFlight := ∅ }
      (.acceptRequest .ready 100 500 20)
      500
      100
  , runtimeCaseFromStep
      "retire_unobserved_generation"
      "retireGeneration"
      runtimeRouterObserved
      (.retireGeneration 1)
  , runtimeCaseFromStep
      "apply_failed_clears_pending"
      "applyFailed"
      runtimeApplyingChanged
      .applyFailed
  , runtimeCaseFromStep
      "missing_dependency_snapshot_is_not_resolved"
      "resolveVisible"
      { runtimeBoot with
        phase := .resolving
      , observedResolved := some runtimeResolvedMissingDependency
      }
      (.resolveVisible runtimeResolvedMissingDependency)
  ]

def clientAgentReadinessCase
    (name : String)
    (observationKind : String)
    (process : ProcessState)
    (activeGeneration routerGeneration : Generation)
    (runnable unavailable startupDemoted : Bool)
    (runtimeUnavailableReason : RuntimeState.AgentUnavailableReason := .backendTemporarilyUnavailable) :
    ClientAgentReadinessCase :=
  let runnableSet : Finset AgentDid := if runnable then {20} else ∅
  let unavailableSet : Finset AgentDid := if unavailable then {20} else ∅
  let demotedSet : Finset AgentDid := if startupDemoted then {20} else ∅
  let resolved : ResolvedSnapshot :=
    { defaultAgent := 20
    , runnable := runnableSet
    , unavailable := unavailableSet
    , dependenciesSatisfied := runnableSet }
  let state : RuntimeState :=
    { runtimeBoot with
      lastResolved := resolved
    , active := resolved.activate activeGeneration
    , routerObservedGeneration := routerGeneration
    , startupDemoted := demotedSet
    , readyGenerations := {activeGeneration, routerGeneration}
    , liveGenerations := {activeGeneration, routerGeneration}
    , sessionAgent := Function.update runtimeBoot.sessionAgent 100 (some 20) }
  let observation := match observationKind with
    | "observed" =>
        RuntimeState.ClientAgentReadiness.ClientNodeObservation.observed process state
    | "malformed" => .malformed
    | "unsupported_version" => .unsupportedVersion
    | _ => .missing
  let projected := RuntimeState.ClientAgentReadiness.project
    observation 20 runtimeUnavailableReason
  { name
  , observationPresent := observationKind != "missing"
  , observationKind
  , processState := process.toDefraDB
  , activeGeneration
  , routerGeneration
  , runnable
  , unavailable
  , startupDemoted
  , runtimeUnavailableReason := runtimeUnavailableReason.code
  , expectedState := projected.stateString
  , expectedReason := projected.reasonCode
  , expectedRuntimeAdmissible :=
      decide (RuntimeState.AgentAdmissible process state 20)
  }

deriving instance Lean.ToJson for RuntimeState.SessionSelection

structure SessionSelectionInput where
  name : String
  requested : Nat
  visible : Option Nat
  deriving Lean.ToJson

@[conformance_family]
def runtimeSessionSelection : Family SessionSelectionInput RuntimeState.SessionSelection where
  inputs :=
    [ { name := "invisible", requested := 10, visible := none }
    , { name := "matching", requested := 10, visible := some 10 }
    , { name := "mismatched", requested := 10, visible := some 20 } ]
  model := fun i => RuntimeState.sessionSelection i.requested i.visible

structure MaterializedSessionSelectionInput where
  name : String
  publishedWithRequest : Bool
  requested : Nat
  deriving Lean.ToJson

/-- A runtime request written by the pending-request owner, routed by its own
Agent: what the router selects when the session was (or was not) published in
the request's transaction (`RuntimeState.materializedSessionVisible`). -/
@[conformance_family]
def runtimeMaterializedSessionSelection :
    Family MaterializedSessionSelectionInput RuntimeState.SessionSelection where
  inputs :=
    [ { name := "materialized", publishedWithRequest := true, requested := 10 } ]
  model := fun i =>
    RuntimeState.sessionSelection i.requested
      (RuntimeState.materializedSessionVisible i.publishedWithRequest i.requested)

structure ConfigurationAdmissionInput where
  name : String
  visible : Bool
  deferredAt : Nat
  active : Nat
  deriving Lean.ToJson

structure ConfigurationAdmissionOutput where
  expected : RuntimeState.ConfigurationAdmission
  retries : Bool
  deriving Lean.ToJson

@[conformance_family]
def runtimeConfigurationAdmissionDeferral :
    Family ConfigurationAdmissionInput ConfigurationAdmissionOutput where
  inputs :=
    [ { name := "visible", visible := true, deferredAt := 3, active := 3 }
    , { name := "invisible_same_generation", visible := false, deferredAt := 3, active := 3 }
    , { name := "invisible_newer_generation", visible := false, deferredAt := 3, active := 4 }
    , { name := "invisible_older_generation", visible := false, deferredAt := 4, active := 3 } ]
  model := fun i =>
    { expected := RuntimeState.configurationAdmission i.visible
      retries := RuntimeState.retriesDeferred i.deferredAt i.active }

end Conformance.ContractCases
