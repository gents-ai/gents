import Proofs.Session.StreamingResponse.ReasoningDeltas
import Proofs.Conformance.Family

namespace Conformance.ReasoningDeltas

open Lean StreamingResponse.ReasoningDeltas

inductive PartKind where
  | reasoning
  | text
  deriving ToJson

structure PartWire where
  kind : PartKind
  id : Option Nat
  signature : Option Nat
  bytes : List Nat
  deriving ToJson

def PartWire.ofModel : Part → PartWire
  | .reasoning id signature bytes => { kind := .reasoning, id, signature, bytes }
  | .text bytes => { kind := .text, id := none, signature := none, bytes }

def PartWire.toModel (wire : PartWire) : Part :=
  match wire.kind with
  | .reasoning => .reasoning wire.id wire.signature wire.bytes
  | .text => .text wire.bytes

structure DeltaInput where
  name : String
  before : List PartWire
  id : Option Nat
  fragment : List Nat
  deriving ToJson

@[conformance_family]
def coalesce : Family DeltaInput (List PartWire) where
  inputs := cases.map fun c =>
    { name := c.name, before := c.before.map PartWire.ofModel, id := c.id, fragment := c.fragment }
  model := fun i => (appendDelta i.id i.fragment (i.before.map PartWire.toModel)).map PartWire.ofModel

end Conformance.ReasoningDeltas
