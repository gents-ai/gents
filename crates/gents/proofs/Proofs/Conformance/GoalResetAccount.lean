import Proofs.Automation.GoalAutomation.ResetResume
import Proofs.Conformance.Functions.Automation
import Proofs.Conformance.Family

/-! Times are seconds after 2030-01-01T00:00:00Z. The limited call starts at 0
and its provider reports a reset at 3600. -/
namespace Conformance.GoalResetAccount

open Lean GoalAutomation.OperatorResume

deriving instance ToJson for CallKind

def binding : Binding :=
  ⟨1, "owner", "session", 10, 100, some "graph-correlation", some "source",
    some "context", some "workspace-authority", "full-semantic-fingerprint", 20, 1⟩
def limited : Snapshot :=
  ⟨⟨.usageLimited, 0, false, false⟩, 0, none, 10, [], 37, some 1000⟩
def request : Request := ⟨.usageLimited, 0, true, true, true, true, binding⟩
def resetAt : Nat := 3600

structure TraceInput where
  kind : CallKind
  inferenceNamesAccount : Bool
  compactionNamesAccount : Bool
  connectedAt : Int
  resumeAt : List Nat
  deriving ToJson

structure Step where
  resumed : Bool
  status : Goals.Status
  continuations : Nat
  deriving ToJson

def facts (i : TraceInput) (now : Nat) : ResetFacts :=
  ⟨true, some resetAt, 0, now,
    (AccountFacts.mk i.kind i.inferenceNamesAccount i.compactionNamesAccount i.connectedAt).profileNamesAccount 0,
    true, true, true⟩

def run (i : TraceInput) : Snapshot → List Nat → List Step
  | _, [] => []
  | s, now :: rest =>
    let (next, outcome) := resumeBy s (.resetReached (facts i now)) request true
    { resumed := outcome == .created, status := next.goal.status,
      continuations := next.children.length } :: run i next rest

@[conformance_family]
def trace : Family TraceInput (List Step) where
  inputs := [CallKind.inference, .compaction].flatMap fun kind =>
    [(true, true), (false, true), (true, false)].flatMap fun (inference, compaction) =>
      [(-1 : Int), 1].map fun connectedAt =>
        { kind, inferenceNamesAccount := inference, compactionNamesAccount := compaction,
          connectedAt, resumeAt := [resetAt - 1, resetAt, 10 * resetAt] }
  model := fun i => run i limited i.resumeAt

end Conformance.GoalResetAccount
