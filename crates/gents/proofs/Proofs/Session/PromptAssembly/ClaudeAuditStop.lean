/-!
# Claude provider audit: block admission and reasoning stop

The provider audit stream (`AssistantTurnAccumulator::apply_provider_audit` and
`reasoning_stop_index` in gents-loop) admits one block event at a time. Each
block is opened once, in index order; a stop closes it once; a thinking block
that stops seals the reasoning entry it opened, and the stop reports that
entry's index. Text and tool-use blocks never report a sealed reasoning index.

Modelled: text, thinking and tool-use blocks with their start, thinking text,
signature and stop events. Fragment bytes do not decide admission or the sealed
index, so they are not modelled. Redacted blocks are not modelled. Block indices
are provider `u32` values; the model uses `Nat` and the family keeps them small.
-/

namespace PromptAssembly.ClaudeAuditStop

inductive BlockKind where
  | text
  | thinking
  | toolUse
  deriving DecidableEq, Repr

inductive Event where
  | blockStart (index : Nat) (kind : BlockKind)
  | thinkingText (index : Nat)
  | signature (index : Nat)
  | blockStop (index : Nat)
  deriving DecidableEq, Repr

inductive Entry where
  | prose
  | reasoning (signed : Bool)
  deriving DecidableEq, Repr

structure State where
  nextBlock : Nat
  kind : Option BlockKind
  stopped : Bool
  content : List Entry
  deriving DecidableEq, Repr

def State.init : State :=
  { nextBlock := 0, kind := none, stopped := false, content := [] }

def entriesOf : BlockKind → List Entry
  | .text => [.prose]
  | .thinking => [.reasoning false]
  | .toolUse => []

def openBlock (s : State) (index : Nat) (kind : BlockKind) : State :=
  { s with
    nextBlock := index + 1
    kind := some kind
    stopped := false
    content := s.content ++ entriesOf kind }

def admits (s : State) (index : Nat) : Bool :=
  (index + 1 == s.nextBlock) && !s.stopped

def lastEntry (s : State) : Option Entry :=
  s.content.getLast?

def openThinkingText (s : State) : Bool :=
  match lastEntry s with
  | some (.reasoning false) => true
  | _ => false

def openReasoning (s : State) : Bool :=
  match lastEntry s with
  | some (.reasoning _) => true
  | _ => false

def sealReasoning (s : State) : State :=
  { s with content := s.content.dropLast ++ [Entry.reasoning true] }

def step (s : State) : Event → Except Unit State
  | .blockStart index kind =>
      if index == s.nextBlock then .ok (openBlock s index kind) else .error ()
  | .thinkingText index =>
      if admits s index && openThinkingText s then .ok s else .error ()
  | .signature index =>
      if admits s index && openReasoning s then .ok (sealReasoning s) else .error ()
  | .blockStop index =>
      if admits s index then .ok { s with stopped := true } else .error ()

def thinkingStop (s : State) (index : Nat) : Except Unit (Option Nat) :=
  if s.content.length - 1 == index && openReasoning s then .ok (some index) else .error ()

def sealedStop (s : State) (index : Nat) : Except Unit (Option Nat) :=
  match s.kind with
  | some BlockKind.thinking => thinkingStop s index
  | _ => .ok none

inductive Observation where
  | rejected
  | accepted (reasoningStop : Option Nat)
  | refusedStop
  deriving DecidableEq, Repr

def stopObservation (next : State) : Event → Observation
  | .blockStop index =>
      (match sealedStop next index with
        | .ok sealed => .accepted sealed
        | .error _ => .refusedStop)
  | _ => .accepted none

def observe (s : State) (event : Event) : Observation × State :=
  match step s event with
  | .error _ => (.rejected, s)
  | .ok next => (stopObservation next event, next)

def advance (acc : List Observation × State) (event : Event) : List Observation × State :=
  let r := observe acc.2 event
  (acc.1 ++ [r.1], r.2)

def trace (events : List Event) : List Observation :=
  (events.foldl advance ([], State.init)).1

theorem provider_stop_marks_the_exact_reasoning_block_once :
    trace [.blockStart 0 .thinking, .thinkingText 0, .signature 0, .blockStop 0,
      .blockStop 0, .blockStart 1 .text, .blockStop 1] =
    [.accepted none, .accepted none, .accepted none, .accepted (some 0),
      .rejected, .accepted none, .accepted none] := by
  decide

theorem reasoning_stop_refuses_a_position_it_does_not_hold :
    trace [.blockStart 0 .toolUse, .blockStart 1 .thinking, .thinkingText 1,
      .signature 1, .blockStop 1] =
    [.accepted none, .accepted none, .accepted none, .accepted none, .refusedStop] := by
  decide

theorem reasoning_stop_reports_the_sealed_index_after_text :
    trace [.blockStart 0 .text, .blockStop 0, .blockStart 1 .thinking,
      .thinkingText 1, .signature 1, .blockStop 1] =
    [.accepted none, .accepted none, .accepted none, .accepted none,
      .accepted none, .accepted (some 1)] := by
  decide

end PromptAssembly.ClaudeAuditStop
