import Proofs.CanonicalOutput.Execution.AdmissionHandoff
import Proofs.CanonicalOutput.Execution.Examples
import Lean

namespace CanonicalOutput.Execution.AdmissionHandoff.Cases

open Lean CanonicalOutput.Execution.Examples Request.CrossActorAuthorization

def context : Context :=
  ⟨10, 1, 600, 7, "selected-agent", { lifecycle := .claimed, committedBy := some "selected-agent" }⟩

def initial (published : Bool) : State :=
  { world := if published then acceptedToolWorld else world 5, context := context }

def reply (decision : Decision := .run) : Action := .reply 600 "selected-agent" decision

def dispatchActions : List Action := [.beginDispatch, .linearizeDispatch, .observeDispatch]

structure Case where
  name : String
  published : Bool
  actions : List Action

def cases : List Case :=
  [ ⟨"accepted-before-invoke", true, [.offer] ++ dispatchActions ++ [reply, .invoke]⟩
  , ⟨"unpublished", false, [.offer] ++ dispatchActions ++ [reply, .invoke]⟩
  , ⟨"reply-before-dispatch", true, [.offer, reply, .invoke]⟩
  , ⟨"no-reply", true, [.offer] ++ dispatchActions ++ [.invoke]⟩
  , ⟨"duplicate-reply", true, [.offer] ++ dispatchActions ++ [reply, reply, .invoke, .invoke]⟩
  , ⟨"wrong-call", true, [.offer] ++ dispatchActions ++ [.reply 601 "selected-agent" .run, .invoke]⟩
  , ⟨"wrong-agent", true, [.offer] ++ dispatchActions ++ [.reply 600 "requester" .run, .invoke]⟩
  , ⟨"skip-with-hook-effect", true, [.offer] ++ dispatchActions ++ [.beginEffect, .observeEffect, reply .skip, .invoke]⟩
  , ⟨"terminate-after-dispatch", true, [.offer] ++ dispatchActions ++ [reply .terminate, .invoke]⟩
  , ⟨"cancel-waiting-unknown", true, [.offer, .beginDispatch, .stop .cancelled, reply, .invoke]⟩
  , ⟨"cancel-then-commit-observed", true, [.offer, .beginDispatch, .stop .cancelled, .linearizeDispatch, .observeDispatch, reply, .invoke]⟩
  , ⟨"commit-then-cancel-outcome-unobserved", true, [.offer, .beginDispatch, .linearizeDispatch, .stop .cancelled]⟩
  , ⟨"commit-then-cancel-before-observation", true, [.offer, .beginDispatch, .linearizeDispatch, .stop .cancelled, .observeDispatch, reply, .invoke]⟩
  , ⟨"cancel-then-rejection-observed", true, [.offer, .beginDispatch, .stop .cancelled, .rejectDispatch, .observeDispatch, reply, .invoke]⟩
  , ⟨"cancel-during-effect", true, [.offer] ++ dispatchActions ++ [.beginEffect, .stop .cancelled, .observeEffect, reply .skip, .invoke]⟩
  , ⟨"lease-lost-waiting", true, [.offer, .beginDispatch, .stop .leaseLost, reply, .invoke]⟩
  , ⟨"shutdown-waiting", true, [.offer, .beginDispatch, .stop .shutdown, reply, .invoke]⟩
  , ⟨"deadline-waiting", true, [.offer, .beginDispatch, .stop .deadline, reply, .invoke]⟩
  , ⟨"dropped-channel", true, [.offer, .beginDispatch, .stop .channelClosed, reply, .invoke]⟩
  , ⟨"cancel-after-reply", true, [.offer] ++ dispatchActions ++ [reply, .stop .cancelled, .invoke]⟩
  , ⟨"lease-lost-after-reply", true, [.offer] ++ dispatchActions ++ [reply, .stop .leaseLost, .invoke]⟩
  , ⟨"shutdown-after-reply", true, [.offer] ++ dispatchActions ++ [reply, .stop .shutdown, .invoke]⟩
  , ⟨"deadline-after-reply", true, [.offer] ++ dispatchActions ++ [reply, .stop .deadline, .invoke]⟩
  , ⟨"lease-expires-before-native-dispatch", true, [.offer, .beginDispatch, .time 10, .linearizeDispatch, .observeDispatch, reply, .invoke]⟩
  , ⟨"lease-expires-after-commit-before-observation", true, [.offer, .beginDispatch, .linearizeDispatch, .time 10, .observeDispatch, reply, .invoke]⟩
  , ⟨"generation-changes-before-dispatch", true, [.offer, .beginDispatch, .replaceGeneration 8, .linearizeDispatch, .observeDispatch, reply, .invoke]⟩
  , ⟨"generation-changes-after-commit", true, [.offer, .beginDispatch, .linearizeDispatch, .replaceGeneration 8, .observeDispatch, reply, .invoke]⟩
  , ⟨"lease-expires-after-reply-without-observed-stop", true, [.offer] ++ dispatchActions ++ [reply, .time 10, .invoke]⟩ ]

def decisionName : Decision → String
  | .run => "run" | .skip => "skip" | .terminate => "terminate"

def phaseName : Phase → String
  | .initial => "initial" | .waiting => "waiting" | .permitted => "permitted" | .closed => "closed"

def stopName : Stop → String
  | .cancelled => "cancelled" | .leaseLost => "lease_lost" | .shutdown => "shutdown"
  | .deadline => "deadline" | .channelClosed => "channel_closed"

def observationName : Observation → String
  | .notStarted => "not_started" | .unknown => "unknown"
  | .committed => "committed" | .rejected => "rejected"

def actionJson : Action → Json
  | .offer => Json.mkObj [("kind", toJson ("offer" : String))]
  | .beginDispatch => Json.mkObj [("kind", toJson ("begin_dispatch" : String))]
  | .linearizeDispatch => Json.mkObj [("kind", toJson ("linearize_dispatch" : String))]
  | .observeDispatch => Json.mkObj [("kind", toJson ("observe_dispatch" : String))]
  | .rejectDispatch => Json.mkObj [("kind", toJson ("reject_dispatch" : String))]
  | .beginEffect => Json.mkObj [("kind", toJson ("begin_effect" : String))]
  | .observeEffect => Json.mkObj [("kind", toJson ("observe_effect" : String))]
  | .invoke => Json.mkObj [("kind", toJson ("invoke" : String))]
  | .time now => Json.mkObj [("kind", toJson ("time" : String)), ("now", toJson now)]
  | .replaceGeneration generation => Json.mkObj [("kind", toJson ("replace_generation" : String)), ("generation", toJson generation)]
  | .stop reason => Json.mkObj [("kind", toJson ("stop" : String)), ("reason", toJson (stopName reason))]
  | .reply call agent decision => Json.mkObj [("kind", toJson ("reply" : String)),
      ("call", toJson call), ("agent", toJson agent), ("decision", toJson (decisionName decision))]

def author (s : State) : Option String :=
  if s.invoked || s.effectObservation == .committed then
    (outbound s.context.effect "child" "child-session" "child-agent-doc" "child-agent" 0 0).map
      (fun result => result.intent.author)
  else none

theorem accepted_reply_dispatches_as_the_selected_agent :
    let result := run (initial true) ([.offer] ++ dispatchActions ++ [reply, .invoke])
    result.invoked = true ∧ result.acceptedReplies = 1 ∧ author result = some "selected-agent" := by
  native_decide

theorem unpublished_or_cancelled_never_invokes_guest :
    (run (initial false) ([.offer] ++ dispatchActions ++ [reply, .invoke])).invoked = false ∧
    (run (initial true) [.offer, .beginDispatch, .stop .cancelled, .linearizeDispatch, .observeDispatch, reply, .invoke]).invoked = false := by
  native_decide

theorem skipped_hook_can_commit_an_agent_authored_effect :
    let result := run (initial true)
      ([.offer] ++ dispatchActions ++ [.beginEffect, .observeEffect, reply .skip, .invoke])
    result.effectObservation = .committed ∧ result.invoked = false ∧
      author result = some "selected-agent" := by
  native_decide

theorem dropped_dispatch_remains_unknown_until_observed :
    (run (initial true) [.offer, .beginDispatch, .stop .cancelled]).dispatchObservation = .unknown := by
  native_decide

theorem elapsed_time_alone_does_not_fence_guest_invocation :
    (run (initial true) ([.offer] ++ dispatchActions ++ [reply, .time 10, .invoke])).invoked = true := by
  native_decide

theorem dispatch_checks_lease_at_linearization :
    (run (initial true) [.offer, .beginDispatch, .time 10, .linearizeDispatch, .observeDispatch]).dispatchObservation = .rejected ∧
    (run (initial true) [.offer, .beginDispatch, .linearizeDispatch, .time 10, .observeDispatch]).dispatchObservation = .committed := by
  native_decide

theorem dispatch_checks_generation_at_linearization :
    (run (initial true) [.offer, .beginDispatch, .replaceGeneration 8, .linearizeDispatch, .observeDispatch]).dispatchObservation = .rejected ∧
    (run (initial true) [.offer, .beginDispatch, .linearizeDispatch, .replaceGeneration 8, .observeDispatch]).dispatchObservation = .committed := by
  native_decide

def caseJson (c : Case) : Json :=
  let result := run (initial c.published) c.actions
  Json.mkObj [("name", toJson c.name), ("published", toJson c.published),
    ("request", toJson context.request), ("session", toJson context.session),
    ("call", toJson context.call), ("generation", toJson context.generation),
    ("selected_agent", toJson context.selectedAgent), ("actions", toJson (c.actions.map actionJson)),
    ("expected_phase", toJson (phaseName result.phase)),
    ("expected_replies", toJson result.acceptedReplies),
    ("expected_decision", toJson (result.decision.map decisionName)),
    ("expected_dispatch", toJson (observationName result.dispatchObservation)),
    ("expected_native_dispatch_committed", toJson result.nativeDispatchCommitted),
    ("expected_effect", toJson (observationName result.effectObservation)),
    ("expected_invoked", toJson result.invoked), ("expected_author", toJson (author result))]

def casesJson : String := (toJson (cases.map caseJson)).compress

end CanonicalOutput.Execution.AdmissionHandoff.Cases
