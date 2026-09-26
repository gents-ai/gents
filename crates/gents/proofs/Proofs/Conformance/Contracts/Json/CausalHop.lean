import Proofs.Request.CausalHop
import Proofs.Conformance.Contracts.Json.Helpers

/-! Executable causal-hop rows. Every expectation evaluates `CausalHop.nextHop`
and `CausalHop.admitHop`; the native materializer (the single writer of
`AgentRequest.subagent_depth`) and the admission hop check must agree with
them. -/
namespace Conformance.CausalHopContracts

open Conformance.Contracts

def causeString : CausalHop.Cause → String
  | .root => "root"
  | .toolCall => "tool_call"
  | .continuation => "continuation"

/-- One materialization step: the hop a request of `cause` receives from its
predecessor's hop, admitted against the target's `max_request_hop`. -/
structure StepCase where
  name : String
  cause : CausalHop.Cause
  predecessorHop : Nat
  maxRequestHop : Nat

/-- A message chain from a root: each cause is materialized from the previous
request's hop and checked against the same target bound. -/
structure ChainCase where
  name : String
  maxRequestHop : Nat
  causes : List CausalHop.Cause

def stepCases : List StepCase :=
  [ ⟨"root_restarts_at_zero", .root, 5, CausalHop.defaultMaxRequestHop⟩
  , ⟨"tool_call_is_one_further", .toolCall, 0, CausalHop.defaultMaxRequestHop⟩
  , ⟨"continuation_copies_hop", .continuation, 3, CausalHop.defaultMaxRequestHop⟩
  , ⟨"tool_call_reaching_bound_is_admitted", .toolCall, 7, CausalHop.defaultMaxRequestHop⟩
  , ⟨"tool_call_beyond_bound_is_refused", .toolCall, 8, CausalHop.defaultMaxRequestHop⟩
  , ⟨"continuation_at_bound_stays_admitted", .continuation, 8, CausalHop.defaultMaxRequestHop⟩
  , ⟨"zero_bound_admits_root", .root, 0, 0⟩
  , ⟨"zero_bound_refuses_first_send", .toolCall, 0, 0⟩
  , ⟨"configured_bound_refuses_past_target_limit", .toolCall, 2, 2⟩ ]

/-- A two-agent message loop with interleaved steering and wakes: the send that
would exceed the default bound is the first refused request. -/
def chainCases : List ChainCase :=
  [ ⟨"message_loop_is_cut_at_default_bound", CausalHop.defaultMaxRequestHop,
      [.root, .toolCall, .continuation, .toolCall, .toolCall, .continuation,
        .toolCall, .toolCall, .toolCall, .continuation, .toolCall, .toolCall,
        .toolCall]⟩
  , ⟨"continuations_never_extend_a_chain", 1,
      [.root, .toolCall, .continuation, .continuation, .continuation]⟩ ]

def stepCaseJson (c : StepCase) : String :=
  let hop := CausalHop.nextHop c.cause c.predecessorHop
  "{\"name\":" ++ jsonString c.name
    ++ ",\"cause\":" ++ jsonString (causeString c.cause)
    ++ ",\"predecessor_hop\":" ++ toString c.predecessorHop
    ++ ",\"max_request_hop\":" ++ toString c.maxRequestHop
    ++ ",\"expected_hop\":" ++ toString hop
    ++ ",\"expected_admitted\":" ++ toString (CausalHop.admitHop c.maxRequestHop hop) ++ "}"

/-- Hops reached after each cause of a chain, starting from hop zero. -/
def chainHops : Nat → List CausalHop.Cause → List Nat
  | _, [] => []
  | start, cause :: rest =>
      let hop := CausalHop.nextHop cause start
      hop :: chainHops hop rest

def chainCaseJson (c : ChainCase) : String :=
  let hops := chainHops 0 c.causes
  "{\"name\":" ++ jsonString c.name
    ++ ",\"max_request_hop\":" ++ toString c.maxRequestHop
    ++ ",\"causes\":" ++ jsonArray (c.causes.map (jsonString ∘ causeString))
    ++ ",\"expected_hops\":" ++ jsonArray (hops.map toString)
    ++ ",\"expected_admitted\":"
      ++ jsonArray (hops.map (toString ∘ CausalHop.admitHop c.maxRequestHop)) ++ "}"

def contractJson : String :=
  "{\"default_max_request_hop\":" ++ toString CausalHop.defaultMaxRequestHop
    ++ ",\"step_cases\":" ++ jsonArray (stepCases.map stepCaseJson)
    ++ ",\"chain_cases\":" ++ jsonArray (chainCases.map chainCaseJson) ++ "}"

/-- The fixture exercises every cause and both admission outcomes. -/
theorem step_cases_cover_causes_and_outcomes :
    [CausalHop.Cause.root, .toolCall, .continuation].all (fun cause =>
      stepCases.any (·.cause == cause)) = true ∧
    stepCases.any (fun c => CausalHop.admitHop c.maxRequestHop
      (CausalHop.nextHop c.cause c.predecessorHop)) = true ∧
    stepCases.any (fun c => !CausalHop.admitHop c.maxRequestHop
      (CausalHop.nextHop c.cause c.predecessorHop)) = true := by
  native_decide

/-- The loop fixture is cut: its final send is refused while every earlier
request is admitted. -/
theorem message_loop_fixture_is_cut :
    (chainCases.head?.map fun c =>
      let admitted := (chainHops 0 c.causes).map (CausalHop.admitHop c.maxRequestHop)
      admitted.getLast? == some false &&
        (admitted.dropLast.all (· == true))) = some true := by
  native_decide

end Conformance.CausalHopContracts
