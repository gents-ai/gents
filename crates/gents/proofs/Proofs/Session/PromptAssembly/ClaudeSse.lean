import Proofs.Session.PromptAssembly.ClaudeMap
import Lean

/-!
# Claude Messages SSE dispatch

`MessagesSseState` dispatches one decoded payload at a time over the content
fold of `ClaudeMap.step`. Each dispatch yields the stream items it completes
and the audit observations it records; the end of the body (`message_stop` or
EOF) flushes an open tool block before the single final response. A tool
call's accumulated input must parse as JSON, and a `refusal` stop reason is a
distinct terminal failure.
-/

namespace PromptAssembly.ClaudeSse

open PromptAssembly.ClaudeMap

inductive Payload where
  | toolStart (index : Nat) (id : ToolExecution.ToolCallId) (name : String) (input : Option String)
  | inputDelta (index : Nat) (fragment : String)
  | thinkingStart (index : Nat) (text signature : String)
  | thinkingDelta (index : Nat) (fragment : String)
  | signatureDelta (index : Nat) (fragment : String)
  | blockStop (index : Nat)
  | stopReason (reason : String)
  | messageStop
  deriving DecidableEq, Repr

inductive Emitted where
  | message (text : String)
  | reasoningDelta (text : String)
  | reasoning (text signature : String)
  | toolCall (id : ToolExecution.ToolCallId) (name arguments : String)
  | final
  deriving DecidableEq, Repr

inductive BlockKind where
  | thinking
  | toolUse
  deriving DecidableEq, Repr

inductive Audit where
  | blockStart (index : Nat) (kind : BlockKind)
  | thinkingText (index : Nat) (fragment : String)
  | signature (index : Nat) (fragment : String)
  | blockStop (index : Nat)
  deriving DecidableEq, Repr

inductive Failure where
  | content (error : MapError)
  | malformedToolInput (id : ToolExecution.ToolCallId)
  | refusal
  deriving DecidableEq, Repr

structure State where
  stream : StreamState
  finished : Bool

structure Step where
  emitted : List Emitted
  audits : List Audit
  deriving DecidableEq, Repr

def State.init : State := { stream := StreamState.init, finished := false }

/-- A sealed block as the stream item it yields; a tool call carries its input
re-encoded from the parsed JSON, so input that does not parse fails closed. -/
def emit : StreamBlock → Except Failure Emitted
  | .text value => .ok (.message value)
  | .reasoning [.text text (some signature)] => .ok (.reasoning text signature)
  | .reasoning _ => .error (.content .unsupportedReasoning)
  | .toolUse id name arguments =>
      match Lean.Json.parse arguments with
      | .ok value => .ok (.toolCall id name value.compress)
      | .error _ => .error (.malformedToolInput id)

def advance (surface : Surface) (st : State) (events : List StreamEvent) :
    Except Failure (State × List Emitted) := do
  let next ← (events.foldlM (step surface) st.stream).mapError Failure.content
  let sealed ← (next.content.drop st.stream.content.length).mapM emit
  return ({ st with stream := next }, sealed)

def close (surface : Surface) (st : State) : Except Failure (State × List Emitted) := do
  let flushed ← (flush surface st.stream).mapError Failure.content
  let sealed ← (flushed.content.drop st.stream.content.length).mapM emit
  if st.finished then return ({ st with stream := flushed }, sealed)
  else return ({ stream := flushed, finished := true }, sealed ++ [.final])

def nonempty {α : Type} (text : String) (value : α) : List α := if text == "" then [] else [value]

def dispatch (surface : Surface) (st : State) : Payload → Except Failure (State × Step)
  | .toolStart index id name input => do
      let (next, emitted) ← advance surface st [.start id name input]
      return (next, ⟨emitted, [.blockStart index .toolUse]⟩)
  | .inputDelta _ fragment => do
      let (next, emitted) ← advance surface st [.delta fragment]
      return (next, ⟨emitted, []⟩)
  | .thinkingStart index text signature => do
      let (next, emitted) ← advance surface st
        ([.thinkingStart index text] ++ nonempty signature (.signatureDelta index signature))
      return (next, ⟨nonempty text (.reasoningDelta text) ++ emitted,
        [.blockStart index .thinking] ++ nonempty text (.thinkingText index text) ++
          nonempty signature (.signature index signature)⟩)
  | .thinkingDelta index fragment => do
      let (next, emitted) ← advance surface st [.thinkingDelta index fragment]
      return (next, ⟨nonempty fragment (.reasoningDelta fragment) ++ emitted,
        nonempty fragment (.thinkingText index fragment)⟩)
  | .signatureDelta index fragment => do
      let (next, emitted) ← advance surface st [.signatureDelta index fragment]
      return (next, ⟨emitted, [.signature index fragment]⟩)
  | .blockStop index => do
      let events : List StreamEvent := match st.stream.pending with
        | none => []
        | some (.tool _) => [.stop]
        | some _ => [.contentStop index]
      let (next, emitted) ← advance surface st events
      return (next, ⟨emitted, [.blockStop index]⟩)
  | .stopReason reason =>
      if reason == "refusal" then .error .refusal else .ok (st, ⟨[], []⟩)
  | .messageStop => do
      let (next, emitted) ← close surface st
      return (next, ⟨emitted, []⟩)

structure Outcome where
  steps : List Step
  finish : List Emitted
  failure : Option Failure
  deriving DecidableEq, Repr

def runFrom (surface : Surface) : State → List Payload → List Step → Outcome
  | st, [], steps =>
      match close surface st with
      | .ok (_, emitted) => ⟨steps, emitted, none⟩
      | .error failure => ⟨steps, [], some failure⟩
  | st, payload :: rest, steps =>
      match dispatch surface st payload with
      | .ok (next, observed) => runFrom surface next rest (steps ++ [observed])
      | .error failure => ⟨steps, [], some failure⟩

def run (surface : Surface) (payloads : List Payload) : Outcome :=
  runFrom surface State.init payloads []

theorem sse_tool_use_with_unparseable_input_fails_closed :
    (run {"echo"} [.toolStart 0 1 "echo" (some "{}"), .inputDelta 0 "{\"text\":",
      .blockStop 0, .messageStop]).failure = some (.malformedToolInput 1) := by
  native_decide

theorem sse_message_stop_flushes_pending_tool_before_final :
    run {"echo"} [.toolStart 0 1 "echo" (some "{}"), .inputDelta 0 "{\"text\":\"hi\"}",
      .messageStop] =
      ⟨[⟨[], [.blockStart 0 .toolUse]⟩, ⟨[], []⟩,
        ⟨[.toolCall 1 "echo" "{\"text\":\"hi\"}", .final], []⟩], [], none⟩ := by
  native_decide

theorem thinking_start_emits_ordered_audit_observations :
    (run ∅ [.thinkingStart 0 "start" "initial", .signatureDelta 0 "-delta", .blockStop 0]).steps.map
        (·.audits) =
      [[.blockStart 0 .thinking, .thinkingText 0 "start", .signature 0 "initial"],
       [.signature 0 "-delta"], [.blockStop 0]] := by
  native_decide

theorem refusal_stop_reason_surfaces_as_a_distinct_error :
    run ∅ [.stopReason "refusal", .messageStop] = ⟨[], [], some .refusal⟩ := by
  native_decide

end PromptAssembly.ClaudeSse
