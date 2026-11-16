import Lean

namespace Runtime.ClaudeSessionLineage

open Lean

inductive Cause where
  | client | agentNew | taskTrigger | pluginJob
  deriving BEq, Repr, ToJson

/-- A verified starter link names the starting session and, separately, the
request in it that caused this session. -/
structure Link where
  session : Nat
  request : Nat
  deriving Repr, ToJson

structure Row where
  session : Nat
  cause : Cause
  starter : Option Link
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
    | true, some link =>
      match rows.find? (·.session == link.session) with
      | none => ⟨cause, true, link.session, parent, some link.request⟩
      | some nextRow =>
        let parent := if fuel == 31 && subagent nextRow.cause then some link.session else parent
        descend rows cause fuel link.session nextRow parent (some link.request)
    | _, _ => ⟨cause, false, session, parent, prompt⟩

def walk (input : Input) : Observation :=
  match input.rows.find? (·.session == input.session) with
  | none => ⟨.client, true, input.session, none, none⟩
  | some row => descend input.rows row.cause 32 input.session row none none

end Runtime.ClaudeSessionLineage
