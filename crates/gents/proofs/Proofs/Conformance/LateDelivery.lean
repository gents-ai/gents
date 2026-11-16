import Proofs.Session.CanonicalOutput.LateDelivery
import Proofs.Conformance.Family

namespace Conformance.LateDelivery

open Lean CanonicalOutput CanonicalOutput.LateDelivery

/-- An authored user message whose text is flushed in `parts`. -/
structure MessageWire where
  key : Nat
  parts : List String
  deriving ToJson

inductive StepKind where
  | deliver | read
  deriving ToJson, DecidableEq, Repr

/-- `deliver` replicates flush `ordinal` of `message`; `read` reconstructs it. -/
structure StepWire where
  kind : StepKind
  message : Nat
  ordinal : Nat
  deriving ToJson

structure TraceInput where
  messages : List MessageWire
  steps : List StepWire
  deriving ToJson

/-- A reconstructed message: its role and each block, a text block as its text. -/
structure ReadWire where
  role : String
  blocks : List String
  deriving ToJson

def segmentId (message ordinal : Nat) : Nat := message * 16 + ordinal + 1

def MessageWire.closeId (m : MessageWire) (index : Nat) : Nat :=
  segmentId index (m.parts.length - 1)

def step (messages : List MessageWire) (s : StepWire) : Option Step :=
  (messages[s.message]?).map fun m => match s.kind with
    | .deliver => .deliver (authoredSegment (segmentId s.message s.ordinal) 0 m.key 0 m.parts s.ordinal)
    | .read => .read (authoredUserMessage (1000 + s.message) 0 m.key 0 (m.closeId s.message)
        (s.message + 1))

def roleName : MessageRole → String
  | .system => "system"
  | .user => "user"
  | .assistant => "assistant"

def blockName : MessageBlock (List UInt8) → String
  | .text bytes => (utf8? bytes).getD ""
  | _ => "non-text"

def observe (result : Except MessageError ReconstructedMessage) : Option ReadWire :=
  result.toOption.map fun m => { role := roleName m.role, blocks := m.blocks.map blockName }

def deliverStep (message ordinal : Nat) : StepWire := { kind := .deliver, message, ordinal }
def readStep (message : Nat) : StepWire := { kind := .read, message, ordinal := 0 }

@[conformance_family]
def reads : Family TraceInput (List (Option ReadWire)) where
  inputs :=
    [ { messages := [⟨1, ["first"]⟩, ⟨2, ["hel", "lo"]⟩]
        steps := [deliverStep 0 0, deliverStep 1 1, readStep 0, deliverStep 1 0, readStep 1] }
    , { messages := [⟨1, ["first"]⟩, ⟨2, ["hel", "lo"]⟩, ⟨3, ["x", "y", "z"]⟩]
        steps := [deliverStep 0 0, deliverStep 2 2, deliverStep 2 0, readStep 0, deliverStep 1 1, deliverStep 1 0,
          readStep 1, deliverStep 2 1, readStep 2, readStep 0, readStep 1] } ]
  model := fun i => (run [] (i.steps.filterMap (step i.messages))).map observe

end Conformance.LateDelivery
