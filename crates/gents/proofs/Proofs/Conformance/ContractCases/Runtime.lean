import Proofs.Runtime.RuntimeReconcile
import Lean
import Mathlib.Data.Finset.Sort
import Proofs.Conformance.ContractCases.Types
import Proofs.Conformance.Family

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
    (runtimeUnavailableReason : RuntimeState.AgentUnavailableReason := .backendTemporarilyUnavailable)
    (storageUnavailable : Bool := false) :
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
  let rowObservation := match observationKind with
    | "observed" =>
        RuntimeState.ClientAgentReadiness.ClientNodeObservation.observed process state
    | "malformed" => .malformed
    | "unsupported_version" => .unsupportedVersion
    | _ => .missing
  let observation := rowObservation.joinStorageFault storageUnavailable
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
  , storageUnavailable
  , expectedState := projected.stateString
  , expectedReason := projected.reasonCode
  , expectedRuntimeAdmissible :=
      decide (RuntimeState.AgentAdmissible process state 20)
  }

/-- The process state a restarting runtime's claim leaves on a predecessor's
fully ready row, derived by running the model's claim rather than stated, so a
claim that stopped demoting would flip the readiness case below. -/
def restartClaimedProcessState : ProcessState :=
  (ProcessState.step? .ready .restartClaim).getD .ready

def clientAgentReadinessCases : List ClientAgentReadinessCase :=
  [ clientAgentReadinessCase "runtime_ready_same_generation" "observed" .ready 4 4 true false false
  , clientAgentReadinessCase "runtime_explicitly_unavailable" "observed" .ready 4 4 false true false
  , clientAgentReadinessCase "runtime_unavailable_wins_overlap" "observed" .ready 4 4 true true false
  , clientAgentReadinessCase "startup_demotion_overrides_runnable" "observed" .ready 4 4 true false true
  , clientAgentReadinessCase "missing_runtime_observation" "missing" .ready 4 4 true false false
  , clientAgentReadinessCase "malformed_runtime_observation" "malformed" .ready 4 4 true false false
  , clientAgentReadinessCase "unsupported_runtime_observation" "unsupported_version" .ready 4 4 true false false
  , clientAgentReadinessCase "runtime_process_recovering" "observed" .recovering 4 4 true false false
  , clientAgentReadinessCase "runtime_process_uninitialized" "observed" .uninitialized 4 4 true false false
  , clientAgentReadinessCase "runtime_process_shutting_down" "observed" .shuttingDown 4 4 true false false
  , clientAgentReadinessCase "runtime_process_shutdown" "observed" .shutdown 4 4 true false false
  , clientAgentReadinessCase "router_generation_stale" "observed" .ready 5 4 true false false
  , clientAgentReadinessCase "zero_generation_is_stale" "observed" .ready 0 0 true false false
  , clientAgentReadinessCase "agent_absent_from_runtime_projection" "observed" .ready 4 4 false false false
  , clientAgentReadinessCase "disabled_agent_is_unavailable" "observed" .ready 4 4 false true false .agentDisabled
  , clientAgentReadinessCase "invalid_runtime_configuration_is_unavailable" "observed" .ready 4 4 false true false .runtimeConfigurationInvalid
  , clientAgentReadinessCase "missing_backend_is_unavailable" "observed" .ready 4 4 false true false .backendNotConfigured
  , clientAgentReadinessCase "disabled_backend_is_unavailable" "observed" .ready 4 4 false true false .backendDisabled
  , clientAgentReadinessCase "missing_credential_is_unavailable" "observed" .ready 4 4 false true false .credentialsRequired
  , clientAgentReadinessCase "rejected_subscription_credential_is_unavailable" "observed" .ready 4 4 false true false .credentialsRequired
  , clientAgentReadinessCase "subscription_not_entitled_is_unavailable" "observed" .ready 4 4 false true false .subscriptionNotEntitled
  , clientAgentReadinessCase "invalid_inference_profile_is_unavailable" "observed" .ready 4 4 false true false .inferenceProfileInvalid
  , clientAgentReadinessCase "invalid_tool_configuration_is_unavailable" "observed" .ready 4 4 false true false .toolConfigurationInvalid
  , clientAgentReadinessCase "invalid_tool_surface_is_unavailable" "observed" .ready 4 4 false true false .toolSurfaceUnavailable
  , clientAgentReadinessCase "restart_claim_demotes_stale_ready" "observed" restartClaimedProcessState 4 4 true false false
  , clientAgentReadinessCase "storage_fault_closes_a_ready_row" "observed" .ready 4 4 true false false
      (storageUnavailable := true)
  , clientAgentReadinessCase "storage_fault_closes_a_missing_row" "missing" .ready 4 4 true false false
      (storageUnavailable := true)
  ]

/-- Settings a generation can hold, spanning an unset selection, an explicit
effort and an explicit Fast tier. -/
def runtimeSettingsLow : EffectiveSettings :=
  { selected := { backendId := "backend-a", model := "model-a", effort := some .low }
  , tier := none }

def runtimeSettingsHighFast : EffectiveSettings :=
  { selected := { backendId := "backend-a", model := "model-b", effort := some .high }
  , tier := some .fast }

def runtimeSettingsUnset : EffectiveSettings :=
  { selected := { backendId := "backend-a", model := "model-c", effort := none }
  , tier := none }

def bindGenerationSettings
    (s : RuntimeState) (generation : Generation) (agent : AgentDid)
    (settings : EffectiveSettings) : RuntimeState :=
  { s with
    generationSettings := fun g a =>
      if g = generation ∧ a = agent then some settings else s.generationSettings g a }

/-- Generation 1 is active and observed, holding the Low selection for agent
20; generation 2 is published and observed, holding the High/Fast selection. -/
def runtimeSettingsGeneration1 : RuntimeState :=
  bindGenerationSettings runtimeTwoAgents 1 20 runtimeSettingsLow

def runtimeSettingsGeneration2 : RuntimeState :=
  bindGenerationSettings
    { runtimeSettingsGeneration1 with
      lastResolved := runtimeResolvedB
    , active := runtimeResolvedB.activate 2
    , routerObservedGeneration := 2
    , readyGenerations := {1, 2}
    , liveGenerations := {1, 2} }
    2 20 runtimeSettingsHighFast

/-- A request dispatched under generation 1 and still running while
generation 2 is applied. -/
def runtimeSettingsInFlight : RuntimeState :=
  { runtimeSettingsGeneration1 with
    phase := .applying
  , pendingResolved := some runtimeResolvedB
  , accepted := {500}
  , inFlight := {500}
  , requestGeneration := Function.update runtimeSettingsGeneration1.requestGeneration 500 1
  , requestSession := Function.update runtimeSettingsGeneration1.requestSession 500 100
  , requestAgent := Function.update runtimeSettingsGeneration1.requestAgent 500 20
  , sessionAgent := Function.update runtimeSettingsGeneration1.sessionAgent 100 (some 20)
  , requestSettings :=
      Function.update runtimeSettingsGeneration1.requestSettings 500 (some runtimeSettingsLow) }

def runtimeEffectiveSettingsCase
    (name actionName : String)
    (pre : RuntimeState)
    (action : RuntimeState.Action)
    (trackedRequestId : RequestId) :
    String × String × Bool × Option EffectiveSettings :=
  match RuntimeState.step? pre action with
  | some post => (name, actionName, true, post.requestSettings trackedRequestId)
  | none => (name, actionName, false, pre.requestSettings trackedRequestId)

/-- Each case names the settings an observer may report for the tracked
request after the action. An illegal action leaves the request unaccepted, and
`none` is then the whole answer: not yet bound. -/
def runtimeEffectiveSettingsBindingCases :
    List (String × String × Bool × Option EffectiveSettings) :=
  [ runtimeEffectiveSettingsCase "accept_binds_the_routed_generation_settings"
      "acceptRequest" runtimeSettingsGeneration1 (.acceptRequest .ready 100 500 20) 500
  , runtimeEffectiveSettingsCase "accept_binds_an_unset_selection"
      "acceptRequest" (bindGenerationSettings runtimeTwoAgents 1 20 runtimeSettingsUnset)
      (.acceptRequest .ready 100 500 20) 500
  , runtimeEffectiveSettingsCase "queued_successor_binds_the_newer_generation"
      "acceptRequest" runtimeSettingsGeneration2 (.acceptRequest .ready 100 501 20) 501
  , runtimeEffectiveSettingsCase "unaccepted_request_has_no_settings"
      "acceptRequest" (bindGenerationSettings runtimePublishedBeforeRouter 2 20
        runtimeSettingsHighFast)
      (.acceptRequest .ready 100 500 20) 500
  , runtimeEffectiveSettingsCase "agent_without_bound_settings_binds_nothing"
      "acceptRequest" runtimeTwoAgents (.acceptRequest .ready 100 500 20) 500
  , runtimeEffectiveSettingsCase "in_flight_settings_survive_a_newer_generation"
      "publish" runtimeSettingsInFlight (.publish runtimeResolvedB) 500 ]

theorem effective_settings_binding_cases_pinned :
    runtimeEffectiveSettingsBindingCases.map (fun c => (c.2.2.1, c.2.2.2)) =
      [ (true, some runtimeSettingsLow)
      , (true, some runtimeSettingsUnset)
      , (true, some runtimeSettingsHighFast)
      , (false, none)
      , (true, none)
      , (true, some runtimeSettingsLow) ] := by native_decide

def configurationAdmissionCasesJson : String :=
  let cases : List (String × Bool × Nat × Nat) :=
    [("visible", true, 3, 3), ("invisible_same_generation", false, 3, 3),
     ("invisible_newer_generation", false, 3, 4), ("invisible_older_generation", false, 4, 3)]
  let result (admission : RuntimeState.ConfigurationAdmission) : String :=
    match admission with
    | .claim => "claim"
    | .awaitNewerGeneration => "await_newer_generation"
  (Lean.toJson (cases.map fun (name, visible, deferredAt, active) => Lean.Json.mkObj
    [("name", Lean.toJson name), ("visible", Lean.toJson visible),
     ("deferred_at", Lean.toJson deferredAt), ("active", Lean.toJson active),
     ("expected", Lean.toJson (result (RuntimeState.configurationAdmission visible))),
     ("retries", Lean.toJson (RuntimeState.retriesDeferred deferredAt active))] )).compress

end Conformance.ContractCases

namespace Conformance.RuntimeSession

open Lean RuntimeState

deriving instance ToJson for SessionSelection, SessionSelectionDisposition

structure SelectionInput where
  requested : Nat
  visible : Option Nat
  attempts : Nat
  bound : Nat
  deriving ToJson

structure SelectionOutput where
  selection : SessionSelection
  disposition : SessionSelectionDisposition
  deriving ToJson

@[conformance_family]
def selection : Family SelectionInput SelectionOutput where
  inputs := [
    { requested := 10, visible := none, attempts := 0, bound := 1 },
    { requested := 10, visible := some 10, attempts := 0, bound := 1 },
    { requested := 10, visible := some 20, attempts := 0, bound := 1 },
    { requested := 10, visible := none, attempts := 0, bound := 3 },
    { requested := 10, visible := none, attempts := 3, bound := 3 }
  ]
  model := fun i =>
    let selected := sessionSelection i.requested i.visible
    { selection := selected,
      disposition := sessionSelectionDisposition selected i.attempts i.bound }

end Conformance.RuntimeSession

namespace Configuration

deriving instance Lean.ToJson for ReasoningEffort, SelectedModel
attribute [conformance_vocabulary] ReasoningEffort.wireName

end Configuration

namespace PromptAssembly.ServiceTier

deriving instance Lean.ToJson for Tier
attribute [conformance_vocabulary] Tier.configName

end PromptAssembly.ServiceTier

deriving instance Lean.ToJson for EffectiveSettings

namespace Conformance.RuntimeEffectiveSettings

open Conformance.ContractCases

inductive Scenario where
  | generationOne | unsetSelection | newGeneration | awaitingRouter
  | noGenerationSettings | retiredGeneration
  deriving Lean.ToJson

structure BindingInput where
  scenario : Scenario
  selected : EffectiveSettings
  claimed : Bool
  runtimeGeneration : Nat
  deriving Lean.ToJson

def observe (input : BindingInput) : Option EffectiveSettings :=
  let (pre, action, rid) := match input.scenario with
    | .generationOne | .unsetSelection =>
        (bindGenerationSettings runtimeTwoAgents 1 20 input.selected,
          RuntimeState.Action.acceptRequest .ready 100 500 20, 500)
    | .newGeneration =>
        (bindGenerationSettings runtimeSettingsGeneration2 2 20 input.selected,
          RuntimeState.Action.acceptRequest .ready 100 501 20, 501)
    | .awaitingRouter =>
        (bindGenerationSettings runtimePublishedBeforeRouter 2 20 input.selected,
          RuntimeState.Action.acceptRequest .ready 100 500 20, 500)
    | .noGenerationSettings =>
        (runtimeTwoAgents, RuntimeState.Action.acceptRequest .ready 100 500 20, 500)
    | .retiredGeneration =>
        ({ runtimeSettingsInFlight with
            requestSettings := Function.update runtimeSettingsInFlight.requestSettings 500
              (some input.selected) }, RuntimeState.Action.publish runtimeResolvedB, 500)
  match RuntimeState.step? pre action with
  | some post => post.requestSettings rid
  | none => pre.requestSettings rid

@[conformance_family]
def binding : Family BindingInput (Option EffectiveSettings) where
  inputs := [
    ⟨.generationOne, runtimeSettingsLow, true, 1⟩,
    ⟨.unsetSelection, runtimeSettingsUnset, true, 1⟩,
    ⟨.newGeneration, runtimeSettingsHighFast, true, 2⟩,
    ⟨.awaitingRouter, runtimeSettingsHighFast, false, 2⟩,
    ⟨.noGenerationSettings, runtimeSettingsUnset, false, 1⟩,
    ⟨.retiredGeneration, runtimeSettingsLow, true, 1⟩
  ]
  model := observe

end Conformance.RuntimeEffectiveSettings
