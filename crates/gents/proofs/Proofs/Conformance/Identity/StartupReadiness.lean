import Proofs.Store.Identity.StartupReadiness
import Proofs.Conformance.Family

namespace Conformance.StartupReadiness

open Lean Identity.StartupReadiness

inductive JoinKind where
  | pending | clean | failed
  deriving ToJson

structure JoinWire where
  kind : JoinKind
  message : Option String
  deriving ToJson

structure ObserveInput where
  name : String
  join : JoinWire
  shuttingDown : Bool
  readiness : Bool
  deadline : Bool
  deriving ToJson

inductive ObservedOutcome where
  | wait | awaitJoin | ready | timeout | premature | failed
  deriving ToJson

def JoinWire.ofModel : Join → JoinWire
  | .pending => { kind := .pending, message := none }
  | .clean => { kind := .clean, message := none }
  | .failed message => { kind := .failed, message := some message }

def JoinWire.toModel (j : JoinWire) : Join :=
  match j.kind with
  | .pending => .pending
  | .clean => .clean
  | .failed => .failed (j.message.getD "")

structure ObserveOutput where
  kind : ObservedOutcome
  message : Option String
  deriving ToJson

def ObserveOutput.ofModel : Outcome → ObserveOutput
  | .wait => ⟨.wait, none⟩
  | .awaitJoin => ⟨.awaitJoin, none⟩
  | .ready => ⟨.ready, none⟩
  | .timeout => ⟨.timeout, none⟩
  | .premature => ⟨.premature, none⟩
  | .failed message => ⟨.failed, some message⟩

@[conformance_family]
def observeOutcome : Family ObserveInput ObserveOutput where
  inputs := cases.map fun c =>
    { name := c.name, join := JoinWire.ofModel c.join, shuttingDown := c.shuttingDown,
      readiness := c.readiness, deadline := c.deadline }
  model := fun i =>
    ObserveOutput.ofModel (observe i.join.toModel i.shuttingDown i.readiness i.deadline)

end Conformance.StartupReadiness
