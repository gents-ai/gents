import Proofs.Store.Identity.GrantSets
import Proofs.Conformance.Family

namespace Conformance.RevokeFollowsGrant

open Lean Identity Identity.GrantSets

/-- Who acts in a step: the Agent that owns the shared session, another
baseline Agent of the same Node, or the User the session is shared with. -/
inductive Actor where
  | agent
  | sibling
  | grantee
  deriving DecidableEq, Repr

inductive Action where
  | grant
  | revoke
  | read
  deriving DecidableEq, Repr

structure Step where
  actor : Actor
  action : Action
  deriving DecidableEq, Repr

def Actor.did : Actor → DID
  | .agent => "agent"
  | .sibling => "sibling"
  | .grantee => "grantee"

/-- The shared session is the Agent's own; both Agents hold the projected
Agent baseline, and the grantee only reads. -/
def trace : List Step :=
  [⟨.grantee, .read⟩, ⟨.sibling, .grant⟩, ⟨.agent, .grant⟩, ⟨.grantee, .read⟩,
   ⟨.sibling, .revoke⟩, ⟨.grantee, .read⟩, ⟨.agent, .revoke⟩, ⟨.grantee, .read⟩,
   ⟨.agent, .grant⟩, ⟨.grantee, .read⟩, ⟨.agent, .revoke⟩, ⟨.grantee, .read⟩]

def baseline : String → Bool := (holderOf (receives .agent)).holds

/-- The step's verdict: whether the change was admitted, or whether the
grantee reads the session. -/
def outcome (state : Household.State) (step : Step) : Bool :=
  match step.action with
  | .grant => relationChangeAdmitted baseline state step.actor.did shareRelation false
  | .revoke => relationChangeAdmitted baseline state step.actor.did shareRelation true
  | .read => Household.allowed 5 state step.actor.did "sessions" "agent-session" "read"

def after (state : Household.State) (step : Step) : Household.State :=
  if outcome state step then
    match step.action with
    | .grant => Household.apply state (.grant shareRelation)
    | .revoke => Household.apply state (.revoke shareRelation)
    | .read => state
  else state

def stateBefore (index : Nat) : Household.State :=
  (trace.take index).foldl after shareFixture

def Action.name : Action → String
  | .grant => "grant"
  | .revoke => "revoke"
  | .read => "read"

def stepAt (index : Nat) : Step := trace.getD index ⟨.grantee, .read⟩

/-- `actor` is the role's DID in the model: `agent`, `sibling` or `grantee`. -/
structure StepInput where
  index : Nat
  actor : String
  action : String
  deriving ToJson

/-- Steps are replayed in order against one store; each expectation is the
model's verdict in the state its prefix leaves. -/
@[conformance_family]
def steps : Family StepInput Bool where
  inputs := (List.range trace.length).map fun index =>
    { index, actor := (stepAt index).actor.did, action := (stepAt index).action.name }
  model := fun i => outcome (stateBefore i.index) (stepAt i.index)

theorem trace_shares_then_withdraws :
    (List.range trace.length).map (fun index =>
      outcome (stateBefore index) (stepAt index)) =
    [false, false, true, true, false, true, true, false, true, true, true, false] := by
  native_decide

end Conformance.RevokeFollowsGrant
