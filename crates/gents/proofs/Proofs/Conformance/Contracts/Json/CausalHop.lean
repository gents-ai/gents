import Proofs.Request.CausalHop
import Proofs.DurableLineage
import Proofs.Conformance.Contracts.Json.Helpers

/-! Executable causal-hop rows. Every expectation evaluates `CausalHop.nextHop`
and `CausalHop.admitHop`; the native materializer (the single writer of
`AgentRequest.request_hop`) and the admission hop check must agree with
them. -/
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

/-- One materialization step: the hop a request of `cause` receives from the
hop of the request it continues in its own session, admitted against the
target's `max_request_hop`. -/
structure StepCase where
  name : String
  cause : CausalHop.Cause
  ownPredecessorHop : Nat
  maxRequestHop : Nat

/-- A causal chain from a root at hop zero across sessions: each step is
materialized from the previous request's hop and checked against the same
target bound. -/
structure ChainCase where
  name : String
  maxRequestHop : Nat
  steps : List CausalHop.Step

def stepCases : List StepCase :=
  [ ⟨"root_restarts_at_zero", .root, 5, CausalHop.defaultMaxRequestHop⟩
  , ⟨"new_session_is_one_past_its_cause", .crossSession 0, 0, CausalHop.defaultMaxRequestHop⟩
  , ⟨"busy_session_steering_climbs_past_its_cause", .crossSession 2, 1,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"cross_session_keeps_a_higher_own_hop", .crossSession 1, 5, CausalHop.defaultMaxRequestHop⟩
  , ⟨"continuation_copies_hop", .continuation, 3, CausalHop.defaultMaxRequestHop⟩
  , ⟨"cross_session_reaching_bound_is_admitted", .crossSession 7, 0,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"cross_session_beyond_bound_is_refused", .crossSession 8, 0,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"completion_wake_keeps_caller_hop", .returnEdge, 3, CausalHop.defaultMaxRequestHop⟩
  , ⟨"completion_wake_at_bound_stays_admitted", .returnEdge, 8,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"completion_wake_into_refused_session_is_refused", .returnEdge, 9,
      CausalHop.defaultMaxRequestHop⟩
  , ⟨"continuation_at_bound_stays_admitted", .continuation, 8, CausalHop.defaultMaxRequestHop⟩
  , ⟨"zero_bound_admits_root", .root, 0, 0⟩
  , ⟨"zero_bound_refuses_first_send", .crossSession 0, 0, 0⟩
  , ⟨"configured_bound_refuses_past_target_limit", .crossSession 2, 0, 2⟩ ]

/-- `pingPong n`: two sessions alternately continuing each other from a root,
each step's own predecessor being the request two links back. -/
def pingPongSteps (n : Nat) : List CausalHop.Step :=
  let hops := CausalHop.pingPongHops n
  (List.range n).map fun index => .cross ((hops.take (index + 1)).reverse.drop 1 |>.head?.getD 0)

/-- Agent loops: an A↔B exchange, and a message loop with interleaved
same-session continuations. Each is cut at its target's bound. -/
def chainCases : List ChainCase :=
  [ ⟨"ping_pong_is_cut_at_default_bound", CausalHop.defaultMaxRequestHop, pingPongSteps 9⟩
  , ⟨"message_loop_is_cut_at_default_bound", CausalHop.defaultMaxRequestHop,
      [.cross 0, .cont, .cross 0, .cross 1, .cont, .cross 2, .cross 0, .cross 3,
        .cont, .cross 4, .cross 5, .cross 6]⟩
  , ⟨"continuations_never_extend_a_chain", 1, [.cross 0, .cont, .cont, .cont]⟩ ]

/-- Calls and returns between a caller `a` and a callee `b` from root hop
zero (`CausalHop.run`); each event's hop is admitted against one bound. -/
structure CallCase where
  name : String
  maxRequestHop : Nat
  events : List CausalHop.Event

/-- A ping-pong in which each send's result is returned to its sender. -/
def pingPongWithReturns : Nat → List CausalHop.Event
  | 0 => []
  | n + 1 => pingPongWithReturns n ++
      (if n % 2 == 0 then [.aSendsB, .returnToA] else [.bSendsA, .returnToB])

def callCases : List CallCase :=
  [ ⟨"sequential_calls_to_one_callee_stay_at_depth_one", 1, CausalHop.sequentialCalls 12⟩
  , ⟨"ping_pong_with_returns_is_cut_at_default_bound", CausalHop.defaultMaxRequestHop,
      pingPongWithReturns 9⟩ ]

/-- Hops of the requests a call trace materializes, from `start`. -/
def callHops : CausalHop.Pair → List CausalHop.Event → List Nat
  | _, [] => []
  | p, e :: rest => e.hop p :: callHops (e.apply p) rest

def eventString : CausalHop.Event → String
  | .aSendsB => "a_sends_b"
  | .bSendsA => "b_sends_a"
  | .returnToA => "return_to_a"
  | .returnToB => "return_to_b"

def callCaseJson (c : CallCase) : String :=
  let hops := callHops ⟨0, 0⟩ c.events
  "{\"name\":" ++ jsonString c.name
    ++ ",\"max_request_hop\":" ++ toString c.maxRequestHop
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
    ++ ",\"max_request_hop\":" ++ toString c.maxRequestHop
    ++ ",\"expected_hop\":" ++ toString hop
    ++ ",\"expected_admitted\":" ++ toString (CausalHop.admitHop c.maxRequestHop hop) ++ "}"

/-- Hops reached after each step of a chain, starting from hop zero. -/
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
    ++ ",\"max_request_hop\":" ++ toString c.maxRequestHop
    ++ ",\"steps\":" ++ jsonArray (c.steps.map stepJson)
    ++ ",\"expected_hops\":" ++ jsonArray (hops.map toString)
    ++ ",\"expected_admitted\":"
      ++ jsonArray (hops.map (toString ∘ CausalHop.admitHop c.maxRequestHop)) ++ "}"

/-- One agent-interrupt permission question (`DurableLineage.interruptAllowed`). -/
structure InterruptCase where
  name : String
  callerSession : String
  targetSession : String
  targetOriginCause : Option String

def interruptCases : List InterruptCase :=
  [ ⟨"spawner_may_interrupt", "caller", "started", some "caller"⟩
  , ⟨"another_session_started_it", "caller", "started", some "other"⟩
  , ⟨"target_started_the_caller", "caller", "parent", none⟩
  , ⟨"messaged_root_session", "caller", "root", none⟩
  , ⟨"own_session", "caller", "caller", some "caller"⟩ ]

def interruptCaseJson (c : InterruptCase) : String :=
  "{\"name\":" ++ jsonString c.name
    ++ ",\"caller_session\":" ++ jsonString c.callerSession
    ++ ",\"target_session\":" ++ jsonString c.targetSession
    ++ ",\"target_origin_cause\":"
      ++ (match c.targetOriginCause with | some s => jsonString s | none => "null")
    ++ ",\"expected_allowed\":"
      ++ toString (DurableLineage.interruptAllowed c.callerSession c.targetSession
        c.targetOriginCause) ++ "}"

/-- The interrupt fixture covers both verdicts. -/
theorem interrupt_cases_cover_both_verdicts :
    interruptCases.any (fun c => DurableLineage.interruptAllowed c.callerSession
      c.targetSession c.targetOriginCause) = true ∧
    interruptCases.any (fun c => !DurableLineage.interruptAllowed c.callerSession
      c.targetSession c.targetOriginCause) = true := by
  native_decide

/-- One delivered session message (`DurableLineage.sessionMessageWrite`) into
a session whose current hop is `ownHop`. -/
structure WriteCase where
  name : String
  delivery : DurableLineage.Delivery
  callerHop : Nat
  ownHop : Nat

def writeCases : List WriteCase :=
  [ ⟨"idle_session_gets_a_request_past_its_caller", .request, 5, 1⟩
  , ⟨"idle_session_keeps_its_higher_hop", .request, 0, 1⟩
  , ⟨"busy_session_is_steered_past_its_caller", .steering, 3, 1⟩
  , ⟨"busy_session_steering_keeps_its_higher_hop", .steering, 0, 1⟩ ]

def writeCaseJson (c : WriteCase) : String :=
  let write := DurableLineage.sessionMessageWrite c.delivery c.callerHop c.ownHop
  "{\"name\":" ++ jsonString c.name
    ++ ",\"delivery\":" ++ jsonString (match c.delivery with
        | .request => "request" | .steering => "steering")
    ++ ",\"caller_hop\":" ++ toString c.callerHop
    ++ ",\"own_hop\":" ++ toString c.ownHop
    ++ ",\"expected_hop\":" ++ toString write.lineage.requestHop
    ++ ",\"names_caller_tool_call\":" ++ toString write.lineage.hasParentToolCallDocId
    ++ ",\"queued_after_active\":" ++ toString write.queuedAfterActive ++ "}"

def contractJson : String :=
  "{\"default_max_request_hop\":" ++ toString CausalHop.defaultMaxRequestHop
    ++ ",\"step_cases\":" ++ jsonArray (stepCases.map stepCaseJson)
    ++ ",\"chain_cases\":" ++ jsonArray (chainCases.map chainCaseJson)
    ++ ",\"call_cases\":" ++ jsonArray (callCases.map callCaseJson)
    ++ ",\"interrupt_cases\":" ++ jsonArray (interruptCases.map interruptCaseJson)
    ++ ",\"write_cases\":" ++ jsonArray (writeCases.map writeCaseJson) ++ "}"

/-- The fixture exercises every cause and both admission outcomes. -/
theorem step_cases_cover_causes_and_outcomes :
    ["root", "cross_session", "return", "continuation"].all (fun cause =>
      stepCases.any (causeString ·.cause == cause)) = true ∧
    stepCases.any (fun c => CausalHop.admitHop c.maxRequestHop
      (CausalHop.nextHop c.cause c.ownPredecessorHop)) = true ∧
    stepCases.any (fun c => !CausalHop.admitHop c.maxRequestHop
      (CausalHop.nextHop c.cause c.ownPredecessorHop)) = true := by
  native_decide

/-- Every loop fixture is cut: its final request is refused while every
earlier request is admitted. -/
theorem loop_fixtures_are_cut :
    (chainCases.take 2).all (fun c =>
      let admitted := (chainHops 0 c.steps).map (CausalHop.admitHop c.maxRequestHop)
      admitted.getLast? == some false &&
        (admitted.dropLast.all (· == true))) = true := by
  native_decide

/-- The exported ping-pong chain is the model's `pingPongHops`. -/
theorem ping_pong_fixture_matches_model :
    chainHops 0 (pingPongSteps 9) = (CausalHop.pingPongHops 9).drop 1 := by
  native_decide

/-- Sequential calls are all admitted at a bound of one. In the ping-pong the
only refused request is the send past the default bound; the results
returned to each sender, including that refused send's, are admitted. -/
theorem call_fixtures_match_the_bound :
    (callHops ⟨0, 0⟩ (CausalHop.sequentialCalls 12)).all (· ≤ 1) = true ∧
    (callHops ⟨0, 0⟩ (pingPongWithReturns 9)).filter
        (fun hop => !CausalHop.admitHop CausalHop.defaultMaxRequestHop hop) =
      [CausalHop.defaultMaxRequestHop + 1] := by
  native_decide

/-- With its returns erased, the exported ping-pong is the model's
`pingPongHops`. -/
theorem ping_pong_with_returns_erases_to_model :
    callHops ⟨0, 0⟩ ((pingPongWithReturns 9).filter (fun e => !e.isReturn)) =
      (CausalHop.pingPongHops 9).drop 1 := by
  native_decide

end Conformance.CausalHopContracts
