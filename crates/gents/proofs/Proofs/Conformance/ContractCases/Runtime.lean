import Proofs.RuntimeReconcile
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

def runtimeResolvedA : ResolvedSnapshot :=
  { defaultBehavior := 10, runnable := {10}, unavailable := ∅
  , dependenciesSatisfied := {10} }

def runtimeResolvedB : ResolvedSnapshot :=
  { defaultBehavior := 20, runnable := {20}, unavailable := {10}
  , dependenciesSatisfied := {20} }

def runtimeResolvedMissingDependency : ResolvedSnapshot :=
  { defaultBehavior := 20, runnable := {20}, unavailable := {10}
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
  , requestBehavior := Function.update runtimeRouterObserved.requestBehavior 500 20
  , sessionBehavior := Function.update runtimeRouterObserved.sessionBehavior 100 (some 20)
  }

def runtimeCaseFromStep
    (name actionName : String)
    (pre : RuntimeState)
    (action : RuntimeState.Action)
    (trackedRequestId : RequestId := 0)
    (trackedSessionId : SessionId := 0) : RuntimeReconcileCase :=
  match RuntimeState.step? pre action with
  | some post =>
      { name := name
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
      , trackedRequestBehavior := post.requestBehavior trackedRequestId
      , trackedSessionBehavior :=
          match post.sessionBehavior trackedSessionId with
          | some behaviorId => behaviorId
          | none => 0
      }
  | none =>
      { name := name
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
      , trackedRequestBehavior := 0
      , trackedSessionBehavior := 0
      }

def runtimeReconcileCases : List RuntimeReconcileCase :=
  [ runtimeCaseFromStep
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
      (.acceptRequest .ready 100 500)
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
      (.acceptRequest .ready 100 500)
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

def clientBehaviorReadinessCase
    (name : String)
    (observationKind : String)
    (process : ProcessState)
    (activeGeneration routerGeneration : Generation)
    (runnable unavailable startupDemoted : Bool)
    (runtimeUnavailableReason : RuntimeState.RuntimeUnavailableReason := .backendTemporarilyUnavailable) :
    ClientBehaviorReadinessCase :=
  let runnableSet : Finset BehaviorId := if runnable then {20} else ∅
  let unavailableSet : Finset BehaviorId := if unavailable then {20} else ∅
  let demotedSet : Finset BehaviorId := if startupDemoted then {20} else ∅
  let resolved : ResolvedSnapshot :=
    { defaultBehavior := 20
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
    , sessionBehavior := Function.update runtimeBoot.sessionBehavior 100 (some 20) }
  let observation := match observationKind with
    | "observed" =>
        RuntimeState.ClientBehaviorReadiness.ClientRuntimeObservation.observed process state
    | "malformed" => .malformed
    | "unsupported_version" => .unsupportedVersion
    | _ => .missing
  let projected := RuntimeState.ClientBehaviorReadiness.project
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
      decide (RuntimeState.BehaviorAdmissible process state 20)
  }

def clientBehaviorReadinessCases : List ClientBehaviorReadinessCase :=
  [ clientBehaviorReadinessCase "runtime_ready_same_generation" "observed" .ready 4 4 true false false
  , clientBehaviorReadinessCase "runtime_explicitly_unavailable" "observed" .ready 4 4 false true false
  , clientBehaviorReadinessCase "runtime_unavailable_wins_overlap" "observed" .ready 4 4 true true false
  , clientBehaviorReadinessCase "startup_demotion_overrides_runnable" "observed" .ready 4 4 true false true
  , clientBehaviorReadinessCase "missing_runtime_observation" "missing" .ready 4 4 true false false
  , clientBehaviorReadinessCase "malformed_runtime_observation" "malformed" .ready 4 4 true false false
  , clientBehaviorReadinessCase "unsupported_runtime_observation" "unsupported_version" .ready 4 4 true false false
  , clientBehaviorReadinessCase "runtime_process_recovering" "observed" .recovering 4 4 true false false
  , clientBehaviorReadinessCase "runtime_process_uninitialized" "observed" .uninitialized 4 4 true false false
  , clientBehaviorReadinessCase "runtime_process_shutting_down" "observed" .shuttingDown 4 4 true false false
  , clientBehaviorReadinessCase "runtime_process_shutdown" "observed" .shutdown 4 4 true false false
  , clientBehaviorReadinessCase "router_generation_stale" "observed" .ready 5 4 true false false
  , clientBehaviorReadinessCase "zero_generation_is_stale" "observed" .ready 0 0 true false false
  , clientBehaviorReadinessCase "behavior_absent_from_runtime_projection" "observed" .ready 4 4 false false false
  , clientBehaviorReadinessCase "disabled_behavior_is_unavailable" "observed" .ready 4 4 false true false .behaviorDisabled
  , clientBehaviorReadinessCase "invalid_runtime_configuration_is_unavailable" "observed" .ready 4 4 false true false .runtimeConfigurationInvalid
  , clientBehaviorReadinessCase "missing_backend_is_unavailable" "observed" .ready 4 4 false true false .backendNotConfigured
  , clientBehaviorReadinessCase "disabled_backend_is_unavailable" "observed" .ready 4 4 false true false .backendDisabled
  , clientBehaviorReadinessCase "missing_credential_is_unavailable" "observed" .ready 4 4 false true false .credentialsRequired
  , clientBehaviorReadinessCase "invalid_inference_profile_is_unavailable" "observed" .ready 4 4 false true false .inferenceProfileInvalid
  , clientBehaviorReadinessCase "invalid_tool_configuration_is_unavailable" "observed" .ready 4 4 false true false .toolConfigurationInvalid
  , clientBehaviorReadinessCase "invalid_tool_surface_is_unavailable" "observed" .ready 4 4 false true false .toolSurfaceUnavailable
  ]

end Conformance.ContractCases
