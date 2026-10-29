import Proofs.Request.CausalHop
import Proofs.DurableLineage
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.CausalHopContracts

open Conformance.Contracts

def causeString : CausalHop.Cause → String
  | .root => "root"
  | .crossSession _ => "cross_session"
  | .returnEdge => "return"
  | .continuation => "continuation"

def causeHopJson : CausalHop.Cause → String
  | .crossSession causeHop => toString causeHop
  | _ => "null"


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

def eventString : CausalHop.Event → String
  | .aSendsB => "a_sends_b"
  | .bSendsA => "b_sends_a"
  | .returnToA => "return_to_a"
  | .returnToB => "return_to_b"

def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def callCaseJson (c : CallCase) : String :=
  let hops := callHops ⟨0, 0⟩ c.events
  "{\"name\":" ++ jsonString c.name
    ++ ",\"max_request_hop\":" ++ optionalNatJson c.maxRequestHop
    ++ ",\"events\":" ++ jsonArray (c.events.map (jsonString ∘ eventString))
    ++ ",\"expected_hops\":" ++ jsonArray (hops.map toString)
    ++ ",\"expected_admitted\":"
      ++ jsonArray (hops.map (toString ∘ CausalHop.admitHop c.maxRequestHop)) ++ "}"

def stepCaseJson (c : StepCase) : String :=
  let hop := CausalHop.nextHop c.cause c.ownPredecessorHop
  "{\"name\":" ++ jsonString c.name
    ++ ",\"cause\":" ++ jsonString (causeString c.cause)
    ++ ",\"cause_hop\":" ++ causeHopJson c.cause
    ++ ",\"own_predecessor_hop\":" ++ toString c.ownPredecessorHop
    ++ ",\"max_request_hop\":" ++ optionalNatJson c.maxRequestHop
    ++ ",\"expected_hop\":" ++ toString hop
    ++ ",\"expected_admitted\":" ++ toString (CausalHop.admitHop c.maxRequestHop hop) ++ "}"


def chainHops : Nat → List CausalHop.Step → List Nat
  | _, [] => []
  | start, step :: rest =>
      let hop := step.apply start
      hop :: chainHops hop rest

def stepJson : CausalHop.Step → String
  | .cross own => "{\"kind\":\"cross_session\",\"own_predecessor_hop\":" ++ toString own ++ "}"
  | .cont => "{\"kind\":\"continuation\",\"own_predecessor_hop\":null}"

def chainCaseJson (c : ChainCase) : String :=
  let hops := chainHops 0 c.steps
  "{\"name\":" ++ jsonString c.name
    ++ ",\"max_request_hop\":" ++ optionalNatJson c.maxRequestHop
    ++ ",\"steps\":" ++ jsonArray (c.steps.map stepJson)
    ++ ",\"expected_hops\":" ++ jsonArray (hops.map toString)
    ++ ",\"expected_admitted\":"
      ++ jsonArray (hops.map (toString ∘ CausalHop.admitHop c.maxRequestHop)) ++ "}"


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

def creationCaseJson (c : CreationCase) : String :=
  "{\"name\":" ++ jsonString c.name
    ++ ",\"signature_valid\":" ++ toString c.signatureValid
    ++ ",\"scope_matches\":" ++ toString c.scopeMatches
    ++ ",\"physical_matches\":" ++ toString c.physicalMatches
    ++ ",\"cause_matches\":" ++ toString c.causeMatches
    ++ ",\"expected\":" ++ toString (DurableLineage.verifiedCreation
      c.signatureValid (c.scopeMatches && c.physicalMatches) true c.causeMatches) ++ "}"

def jobCreationCases : List (Bool × Bool × Bool × Bool) :=
  [(true, true, true, true), (false, true, true, true),
   (true, false, true, true), (true, true, false, true),
   (true, true, true, false)]

def jobCreationCaseJson (c : Bool × Bool × Bool × Bool) : String :=
  let (parent, job, key, spawn) := c
  "{\"parent\":" ++ toString parent ++ ",\"job\":" ++ toString job
    ++ ",\"key\":" ++ toString key ++ ",\"spawn\":" ++ toString spawn
    ++ ",\"expected\":" ++ toString (DurableLineage.verifiedJobCreation parent job key spawn) ++ "}"

def deferredCreationCaseJson (c : Bool × Bool × Bool × Bool) : String :=
  let (kind, receipt, opening, cause) := c
  "{\"kind\":" ++ toString kind ++ ",\"receipt\":" ++ toString receipt
    ++ ",\"opening\":" ++ toString opening ++ ",\"cause\":" ++ toString cause
    ++ ",\"expected\":" ++ toString (DurableLineage.deferredCreationAllowed kind receipt opening cause) ++ "}"

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

def interruptCaseJson (c : InterruptCase) : String :=
  "{\"name\":" ++ jsonString c.name
    ++ ",\"caller_session\":" ++ jsonString c.callerSession
    ++ ",\"target_session\":" ++ jsonString c.targetSession
    ++ ",\"target_origin_cause\":"
      ++ (match c.targetOriginCause with | some s => jsonString s | none => "null")
    ++ ",\"same_node\":" ++ toString c.sameNode
    ++ ",\"task_triggered\":" ++ toString c.taskTriggered
    ++ ",\"expected_allowed\":"
      ++ toString (DurableLineage.interruptAllowed c.callerSession c.targetSession
        c.targetOriginCause c.sameNode c.taskTriggered) ++ "}"


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

def writeCaseJson (c : WriteCase) : String :=
  let write := DurableLineage.sessionMessageWrite c.delivery c.callerHop c.ownHop
  "{\"name\":" ++ jsonString c.name
    ++ ",\"delivery\":" ++ jsonString (match c.delivery with
        | .request => "request" | .queued => "queue" | .steering => "steer")
    ++ ",\"caller_hop\":" ++ toString c.callerHop
    ++ ",\"own_hop\":" ++ toString c.ownHop
    ++ ",\"expected_hop\":" ++ toString write.lineage.requestHop
    ++ ",\"names_caller_tool_call\":" ++ toString write.lineage.hasParentToolCallDocId
    ++ ",\"queued_after_active\":" ++ toString write.queuedAfterActive ++ "}"

structure SessionTargetCase where
  sameRequester : Bool
  sameNode : Bool
  caused : Bool
  allowedTarget : Bool

def sessionTargetCases : List SessionTargetCase := do
  let sameRequester ← [false, true]
  let sameNode ← [false, true]
  let caused ← [false, true]
  let allowedTarget ← [false, true]
  pure ⟨sameRequester, sameNode, caused, allowedTarget⟩

def sessionTargetCaseJson (c : SessionTargetCase) : String :=
  "{\"same_requester\":" ++ toString c.sameRequester
    ++ ",\"same_node\":" ++ toString c.sameNode
    ++ ",\"caused\":" ++ toString c.caused
    ++ ",\"allowed_target\":" ++ toString c.allowedTarget
    ++ ",\"expected_allowed\":" ++ toString (DurableLineage.sessionTargetAllowed
      c.sameRequester c.sameNode c.caused c.allowedTarget) ++ "}"

def contractJson : String :=
  "{\"default_max_request_hop\":" ++ optionalNatJson CausalHop.defaultMaxRequestHop
    ++ ",\"step_cases\":" ++ jsonArray (stepCases.map stepCaseJson)
    ++ ",\"chain_cases\":" ++ jsonArray (chainCases.map chainCaseJson)
    ++ ",\"call_cases\":" ++ jsonArray (callCases.map callCaseJson)
    ++ ",\"interrupt_cases\":" ++ jsonArray (interruptCases.map interruptCaseJson)
    ++ ",\"creation_cases\":" ++ jsonArray (creationCases.map creationCaseJson)
    ++ ",\"job_creation_cases\":" ++ jsonArray (jobCreationCases.map jobCreationCaseJson)
    ++ ",\"deferred_creation_cases\":" ++ jsonArray (jobCreationCases.map deferredCreationCaseJson)
    ++ ",\"write_cases\":" ++ jsonArray (writeCases.map writeCaseJson)
    ++ ",\"session_target_cases\":" ++ jsonArray (sessionTargetCases.map sessionTargetCaseJson) ++ "}"


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

end Conformance.CausalHopContracts
