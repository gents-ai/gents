import Lean

namespace Runtime.ClaudeSessionLineage

open Lean

inductive Cause where
  | client | agentNew | taskTrigger | pluginJob
  deriving BEq, Repr, ToJson

structure Row where
  session : Nat
  cause : Cause
  starter : Option Nat
  deriving Repr, ToJson

structure Input where
  session : Nat
  rows : List Row
  deriving Repr, ToJson

structure Observation where
  cause : Cause
  rootClient : Bool
  mainSession : Nat
  parentSubagent : Option Nat
  mainPrompt : Option Nat
  deriving BEq, Repr, ToJson

def subagent (cause : Cause) : Bool :=
  cause == .agentNew || cause == .pluginJob

/-- Verified receipt reads are supplied by the session owner. A missing receipt
is a client root. The bounded walk reports the deepest verified session after
32 links as background work, including cycles; it cannot infer a client root. -/
def descend (rows : List Row) (cause : Cause) :
    Nat → Nat → Row → Option Nat → Option Nat → Observation
  | 0, session, _, parent, prompt => ⟨cause, false, session, parent, prompt⟩
  | fuel + 1, session, row, parent, prompt =>
    match subagent row.cause, row.starter with
    | true, some next =>
      match rows.find? (·.session == next) with
      | none => ⟨cause, true, next, parent, some next⟩
      | some nextRow =>
        let parent := if fuel == 31 && subagent nextRow.cause then some next else parent
        descend rows cause fuel next nextRow parent (some next)
    | _, _ => ⟨cause, false, session, parent, prompt⟩

def walk (input : Input) : Observation :=
  match input.rows.find? (·.session == input.session) with
  | none => ⟨.client, true, input.session, none, none⟩
  | some row => descend input.rows row.cause 32 input.session row none none

end Runtime.ClaudeSessionLineage
