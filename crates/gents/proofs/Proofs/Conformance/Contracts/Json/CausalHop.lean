import Proofs.Session.Request.CausalHop
import Proofs.Session.DurableLineage
import Proofs.Session.AgentSession
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.CausalHopContracts

open Lean Conformance.Contracts

def causeString : CausalHop.Cause → String
  | .root => "root"
  | .crossSession _ => "cross_session"
  | .returnEdge => "return"
  | .continuation => "continuation"

structure StepCase where
  name : String
  cause : CausalHop.Cause
  ownPredecessorHop : Nat
  maxRequestHop : Option Nat


structure ChainCase where
  name : String
  maxRequestHop : Option Nat
  steps : List CausalHop.Step

def stepCases : List StepCase :=
  [ ⟨"root_restarts_at_zero", .root, 5, CausalHop.defaultMaxRequestHop⟩
  , ⟨"new_session_is_one_past_its_cause", .crossSession 0, 0, CausalHop.defaultMaxRequestHop⟩
  , ⟨"busy_session_steering_climbs_past_its_cause", .crossSession 2, 1,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"cross_session_keeps_a_higher_own_hop", .crossSession 1, 5, CausalHop.defaultMaxRequestHop⟩
  , ⟨"continuation_copies_hop", .continuation, 3, CausalHop.defaultMaxRequestHop⟩
  , ⟨"cross_session_reaching_configured_bound_is_admitted", .crossSession 7, 0,
      some 8⟩
  , ⟨"cross_session_beyond_configured_bound_is_refused", .crossSession 8, 0,
      some 8⟩
  , ⟨"completion_wake_keeps_caller_hop", .returnEdge, 3, CausalHop.defaultMaxRequestHop⟩
  , ⟨"completion_wake_at_bound_stays_admitted", .returnEdge, 8,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"completion_wake_into_refused_session_is_refused", .returnEdge, 9,
      some 8⟩
  , ⟨"continuation_at_bound_stays_admitted", .continuation, 8, CausalHop.defaultMaxRequestHop⟩
  , ⟨"plugin_job_completion", .continuation, 4, CausalHop.defaultMaxRequestHop⟩
  , ⟨"zero_bound_admits_root", .root, 0, some 0⟩
  , ⟨"zero_bound_refuses_first_send", .crossSession 0, 0, some 0⟩
  , ⟨"configured_bound_refuses_past_target_limit", .crossSession 2, 0, some 2⟩
  , ⟨"unlimited_default_admits_deep_hop", .crossSession 99, 0,
      CausalHop.defaultMaxRequestHop⟩ ]


def pingPongSteps (n : Nat) : List CausalHop.Step :=
  let hops := CausalHop.pingPongHops n
  (List.range n).map fun index => .cross ((hops.take (index + 1)).reverse.drop 1 |>.head?.getD 0)


def chainCases : List ChainCase :=
  [ ⟨"ping_pong_is_cut_at_configured_bound", some 8, pingPongSteps 9⟩
  , ⟨"message_loop_is_cut_at_configured_bound", some 8,
      [.cross 0, .cont, .cross 0, .cross 1, .cont, .cross 2, .cross 0, .cross 3,
        .cont, .cross 4, .cross 5, .cross 6]⟩
  , ⟨"continuations_never_extend_a_chain", some 1, [.cross 0, .cont, .cont, .cont]⟩
  , ⟨"unlimited_default_admits_open_chain", CausalHop.defaultMaxRequestHop,
      pingPongSteps 100⟩ ]


structure CallCase where
  name : String
  maxRequestHop : Option Nat
  events : List CausalHop.Event


def pingPongWithReturns : Nat → List CausalHop.Event
  | 0 => []
  | n + 1 => pingPongWithReturns n ++
      (if n % 2 == 0 then [.aSendsB, .returnToA] else [.bSendsA, .returnToB])

def callCases : List CallCase :=
  [ ⟨"sequential_calls_to_one_callee_stay_at_depth_one", some 1, CausalHop.sequentialCalls 12⟩
  , ⟨"ping_pong_with_returns_is_cut_at_configured_bound", some 8,
      pingPongWithReturns 9⟩ ]


def callHops : CausalHop.Pair → List CausalHop.Event → List Nat
  | _, [] => []
  | p, e :: rest => e.hop p :: callHops (e.apply p) rest

deriving instance Lean.ToJson for CausalHop.Event

structure CallInput where
  name : String
  maxRequestHop : Option Nat
  events : List CausalHop.Event
  deriving Lean.ToJson

def chainHops : Nat → List CausalHop.Step → List Nat
  | _, [] => []
  | start, step :: rest =>
      let hop := step.apply start
      hop :: chainHops hop rest

inductive ChainStepKind where
  | crossSession
  | continuation
  deriving Lean.ToJson

structure ChainStepWire where
  kind : ChainStepKind
  ownPredecessorHop : Option Nat
  deriving Lean.ToJson

def ChainStepWire.ofModel : CausalHop.Step → ChainStepWire
  | .cross own => { kind := .crossSession, ownPredecessorHop := some own }
  | .cont => { kind := .continuation, ownPredecessorHop := none }

def ChainStepWire.toModel (step : ChainStepWire) : CausalHop.Step :=
  match step.kind with
  | .crossSession => .cross (step.ownPredecessorHop.getD 0)
  | .continuation => .cont

structure ChainInput where
  name : String
  maxRequestHop : Option Nat
  steps : List ChainStepWire
  deriving Lean.ToJson

structure HopTrace where
  hops : List Nat
  admitted : List Bool
  deriving Lean.ToJson

@[conformance_family]
def chain : Family ChainInput HopTrace where
  inputs := chainCases.map fun c =>
    { name := c.name, maxRequestHop := c.maxRequestHop, steps := c.steps.map ChainStepWire.ofModel }
  model := fun i =>
    let hops := chainHops 0 (i.steps.map ChainStepWire.toModel)
    { hops, admitted := hops.map (CausalHop.admitHop i.maxRequestHop) }


@[conformance_family]
def call : Family CallInput HopTrace where
  inputs := callCases.map fun c =>
    { name := c.name, maxRequestHop := c.maxRequestHop, events := c.events }
  model := fun i =>
    let hops := callHops ⟨0, 0⟩ i.events
    { hops, admitted := hops.map (CausalHop.admitHop i.maxRequestHop) }

structure CreationCase where
  name : String
  signatureValid : Bool
  scopeMatches : Bool
  physicalMatches : Bool
  causeMatches : Bool

def creationCases : List CreationCase :=
  [ ⟨"valid", true, true, true, true⟩
  , ⟨"bad_signature", false, true, true, true⟩
  , ⟨"wrong_scope", true, false, true, true⟩
  , ⟨"recreated_session", true, true, false, true⟩
  , ⟨"wrong_cause", true, true, true, false⟩ ]

deriving instance Lean.ToJson for CreationCase

@[conformance_family]
def creation : Family CreationCase Bool where
  inputs := creationCases
  model := fun c => DurableLineage.verifiedCreation
    c.signatureValid (c.scopeMatches && c.physicalMatches) true c.causeMatches

def creationSignerCases : List (Bool × Bool × String) :=
  [(true, true, "creator"), (true, true, "worker"), (true, true, "other"),
   (true, false, "creator"), (false, true, "creator"), (false, true, "worker")]

def jobCreationCases : List (Bool × Bool × Bool × Bool) :=
  [(true, true, true, true), (false, true, true, true),
   (true, false, true, true), (true, true, false, true),
   (true, true, true, false)]

structure InterruptCase where
  name : String
  callerSession : String
  targetSession : String
  targetOriginCause : Option String
  sameNode : Bool
  taskTriggered : Bool

def interruptCases : List InterruptCase :=
  [ ⟨"spawner_may_interrupt", "caller", "started", some "caller", true, false⟩
  , ⟨"another_session_started_it", "caller", "started", some "other", true, false⟩
  , ⟨"target_started_the_caller", "caller", "parent", none, true, false⟩
  , ⟨"messaged_root_session", "caller", "root", none, true, false⟩
  , ⟨"task_trigger", "caller", "root", none, true, true⟩
  , ⟨"foreign_task_trigger", "caller", "root", none, false, true⟩
  , ⟨"own_session", "caller", "caller", some "caller", true, true⟩ ]

theorem interrupt_cases_cover_both_verdicts :
    interruptCases.any (fun c => DurableLineage.interruptAllowed c.callerSession
      c.targetSession c.targetOriginCause c.sameNode c.taskTriggered) = true ∧
    interruptCases.any (fun c => !DurableLineage.interruptAllowed c.callerSession
      c.targetSession c.targetOriginCause c.sameNode c.taskTriggered) = true := by
  native_decide


structure WriteCase where
  name : String
  delivery : DurableLineage.Delivery
  callerHop : Nat
  ownHop : Nat

def writeCases : List WriteCase :=
  [ ⟨"idle_session_gets_a_request_past_its_caller", .request, 5, 1⟩
  , ⟨"idle_session_keeps_its_higher_hop", .request, 0, 1⟩
  , ⟨"busy_session_is_queued_past_its_caller", .queued, 3, 1⟩
  , ⟨"busy_session_queue_keeps_its_higher_hop", .queued, 0, 1⟩
  , ⟨"busy_session_is_steered_past_its_caller", .steering, 3, 1⟩ ]

inductive WriteDelivery where
  | request
  | queue
  | steer
  deriving Lean.ToJson

def WriteDelivery.ofModel : DurableLineage.Delivery → WriteDelivery
  | .request => .request
  | .queued => .queue
  | .steering => .steer

def WriteDelivery.toModel : WriteDelivery → DurableLineage.Delivery
  | .request => .request
  | .queue => .queued
  | .steer => .steering

structure WriteInput where
  name : String
  delivery : WriteDelivery
  callerHop : Nat
  ownHop : Nat
  deriving Lean.ToJson

structure WriteOutput where
  hop : Nat
  namesCallerToolCall : Bool
  queuedAfterActive : Bool
  deriving Lean.ToJson

@[conformance_family]
def write : Family WriteInput WriteOutput where
  inputs := writeCases.map fun c =>
    { name := c.name, delivery := WriteDelivery.ofModel c.delivery, callerHop := c.callerHop,
      ownHop := c.ownHop }
  model := fun i =>
    let write := DurableLineage.sessionMessageWrite i.delivery.toModel i.callerHop i.ownHop
    { hop := write.lineage.requestHop
      namesCallerToolCall := write.lineage.hasParentToolCallDocId
      queuedAfterActive := write.queuedAfterActive }

structure SessionTargetCase where
  sameRequester : Bool
  sameNode : Bool
  caused : Bool
  allowedTarget : Bool
  deriving Lean.ToJson

def sessionTargetCases : List SessionTargetCase := do
  let sameRequester ← [false, true]
  let sameNode ← [false, true]
  let caused ← [false, true]
  let allowedTarget ← [false, true]
  pure ⟨sameRequester, sameNode, caused, allowedTarget⟩

structure SessionListCase where
  target : SessionTargetCase
  verifiedStarterReceipt : Bool
  interruptAllowed : Bool

def sessionListCases : List SessionListCase := do
  let target ← sessionTargetCases
  let verifiedStarterReceipt ← [false, true]
  let interruptAllowed ← [false, true]
  pure ⟨target, verifiedStarterReceipt, interruptAllowed⟩

structure CapObservationCase where
  name : String
  observation : Option (Option Nat)
  hop : Nat

def capObservationCases : List CapObservationCase :=
  [ ⟨"hidden", none, 1⟩
  , ⟨"missing", none, 1⟩
  , ⟨"unset", some none, 7⟩
  , ⟨"at_bound", some (some 1), 1⟩
  , ⟨"over_bound", some (some 1), 2⟩ ]

structure CapObservationInput where
  name : String
  visible : Bool
  cap : Option Nat
  hop : Nat
  deriving Lean.ToJson

structure CapObservationOutput where
  expected : Option Bool
  bound : Option Nat
  deriving Lean.ToJson

@[conformance_family]
def capObservation : Family CapObservationInput CapObservationOutput where
  inputs := capObservationCases.map fun c =>
    { name := c.name, visible := c.observation.isSome, cap := c.observation.getD none, hop := c.hop }
  model := fun i =>
    let observation := if i.visible then some i.cap else none
    { expected := CausalHop.admitObservedHop observation i.hop
      bound := CausalHop.observedHopBound observation }

structure StepInput where
  name : String
  cause : String
  causeHop : Option Nat
  ownPredecessorHop : Nat
  maxRequestHop : Option Nat
  deriving Lean.ToJson

structure StepOutput where
  hop : Nat
  admitted : Bool
  deriving Lean.ToJson

def causeOfWire : String → Option Nat → CausalHop.Cause
  | "cross_session", some causeHop => .crossSession causeHop
  | "return", _ => .returnEdge
  | "continuation", _ => .continuation
  | _, _ => .root

def causeHopOf : CausalHop.Cause → Option Nat
  | .crossSession causeHop => some causeHop
  | _ => none

def StepInput.ofCase (c : StepCase) : StepInput :=
  { name := c.name
    cause := causeString c.cause
    causeHop := causeHopOf c.cause
    ownPredecessorHop := c.ownPredecessorHop
    maxRequestHop := c.maxRequestHop }

@[conformance_family]
def stepAdmission : Family StepInput StepOutput where
  inputs := stepCases.map StepInput.ofCase
  model := fun i =>
    let hop := CausalHop.nextHop (causeOfWire i.cause i.causeHop) i.ownPredecessorHop
    { hop, admitted := CausalHop.admitHop i.maxRequestHop hop }

structure JobCreationInput where
  parent : Bool
  job : Bool
  key : Bool
  spawn : Bool
  deriving Lean.ToJson

@[conformance_family]
def jobCreation : Family JobCreationInput Bool where
  inputs := jobCreationCases.map fun (parent, job, key, spawn) =>
    { parent, job, key, spawn }
  model := fun i => DurableLineage.verifiedJobCreation i.parent i.job i.key i.spawn

@[conformance_family]
def sessionTarget : Family SessionTargetCase Bool where
  inputs := sessionTargetCases
  model := fun c =>
    DurableLineage.sessionTargetAllowed c.sameRequester c.sameNode c.caused c.allowedTarget

structure SessionListInput where
  sameRequester : Bool
  sameNode : Bool
  caused : Bool
  allowedTarget : Bool
  verifiedStarterReceipt : Bool
  interruptAllowed : Bool
  deriving Lean.ToJson

structure SessionListOutput where
  listed : Bool
  canMessage : Bool
  canInterrupt : Bool
  deriving Lean.ToJson

@[conformance_family]
def sessionList : Family SessionListInput SessionListOutput where
  inputs := sessionListCases.map fun c =>
    { sameRequester := c.target.sameRequester
      sameNode := c.target.sameNode
      caused := c.target.caused
      allowedTarget := c.target.allowedTarget
      verifiedStarterReceipt := c.verifiedStarterReceipt
      interruptAllowed := c.interruptAllowed }
  model := fun i =>
    let entry := DurableLineage.listedSession i.sameRequester i.sameNode i.caused
      i.allowedTarget i.verifiedStarterReceipt i.interruptAllowed
    { listed := entry.isSome
      canMessage := entry.any (·.canMessage)
      canInterrupt := entry.any (·.canInterrupt) }

theorem step_cases_cover_causes_and_outcomes :
    ["root", "cross_session", "return", "continuation"].all (fun cause =>
      stepCases.any (causeString ·.cause == cause)) = true ∧
    stepCases.any (fun c => CausalHop.admitHop c.maxRequestHop
      (CausalHop.nextHop c.cause c.ownPredecessorHop)) = true ∧
    stepCases.any (fun c => !CausalHop.admitHop c.maxRequestHop
      (CausalHop.nextHop c.cause c.ownPredecessorHop)) = true := by
  native_decide


theorem loop_fixtures_are_cut :
    (chainCases.take 2).all (fun c =>
      let admitted := (chainHops 0 c.steps).map (CausalHop.admitHop c.maxRequestHop)
      admitted.getLast? == some false &&
        (admitted.dropLast.all (· == true))) = true := by
  native_decide


theorem ping_pong_fixture_matches_model :
    chainHops 0 (pingPongSteps 9) = (CausalHop.pingPongHops 9).drop 1 := by
  native_decide


theorem call_fixtures_match_the_bound :
    (callHops ⟨0, 0⟩ (CausalHop.sequentialCalls 12)).all (· ≤ 1) = true ∧
    (callHops ⟨0, 0⟩ (pingPongWithReturns 9)).filter
        (fun hop => !CausalHop.admitHop (some 8) hop) =
      [9] := by
  native_decide


theorem ping_pong_with_returns_erases_to_model :
    callHops ⟨0, 0⟩ ((pingPongWithReturns 9).filter (fun e => !e.isReturn)) =
      (CausalHop.pingPongHops 9).drop 1 := by
  native_decide

theorem default_max_request_hop_is_unbounded : CausalHop.defaultMaxRequestHop = none := rfl

structure DefaultCapInput where
  name : String
  deriving ToJson

/-- A Node published without `max_request_hop`, as `gents init` publishes it. -/
@[conformance_family]
def defaultCap : Family DefaultCapInput (Option Nat) where
  inputs := [{ name := "node_without_max_request_hop" }]
  model := fun _ => CausalHop.defaultMaxRequestHop

def signerDid (signer : String) : Nat :=
  if signer == "creator" then 1 else if signer == "worker" then 2 else 3

structure CreationSignerInput where
  agentNew : Bool
  hasRequester : Bool
  signer : String
  deriving ToJson

structure CreationSignerOutput where
  expected : Bool
  pluginJobExpected : Option Bool
  deriving ToJson

def signerAccepts (i : CreationSignerInput) : Bool :=
  let requester := if i.hasRequester then some 1 else none
  AgentSession.creationReceiptSigner i.agentNew requester 2 == some (signerDid i.signer)

@[conformance_family]
def creationSigner : Conformance.Family CreationSignerInput CreationSignerOutput where
  inputs := creationSignerCases.map fun (agentNew, hasRequester, signer) =>
    { agentNew, hasRequester, signer }
  model := fun i =>
    { expected := signerAccepts i
      pluginJobExpected := if i.agentNew then some (signerAccepts i) else none }

structure DeferredCreationInput where
  kind : Bool
  receipt : Bool
  opening : Bool
  cause : Bool
  deriving ToJson

@[conformance_family]
def deferredCreation : Conformance.Family DeferredCreationInput Bool where
  inputs := jobCreationCases.map fun (kind, receipt, opening, cause) =>
    { kind, receipt, opening, cause }
  model := fun i => DurableLineage.deferredCreationAllowed i.kind i.receipt i.opening i.cause

structure InterruptInput where
  callerSession : String
  targetSession : String
  targetOriginCause : Option String
  sameNode : Bool
  taskTriggered : Bool
  deriving ToJson

@[conformance_family]
def interrupt : Conformance.Family InterruptInput Bool where
  inputs := interruptCases.map fun c =>
    { callerSession := c.callerSession, targetSession := c.targetSession,
      targetOriginCause := c.targetOriginCause, sameNode := c.sameNode,
      taskTriggered := c.taskTriggered }
  model := fun i => DurableLineage.interruptAllowed i.callerSession i.targetSession
    i.targetOriginCause i.sameNode i.taskTriggered

end Conformance.CausalHopContracts
