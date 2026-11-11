import Proofs.Hooks.Claim
import Proofs.Hooks.MustCommit
import Proofs.Conformance.ContractTypes

namespace Conformance.HooksContracts
open Hooks
open Conformance.Contracts (jsonString jsonArray jsonStringArray)

abbrev HookEffect := Effect String String String
abbrev HookRow := Row String String
abbrev HookState := State (List String) String String
abbrev Record := Delivery.State String String

def caller : Actor := "did:key:z6MkLifecycleCaller"

/-- A concrete transaction: rows of plugin-owned collections, core state as the
list of operations core performed, the document ACP denials for `caller`, and
the core operations whose `perform` refuses. -/
structure World where
  rows : List HookRow
  core : List String := ["claimed"]
  denied : List (Access × Ref String) := []
  failingOps : List String := []

def World.state (world : World) : HookState := ⟨world.core, world.rows⟩

def World.allowed (world : World) : Acp String :=
  fun _ access ref => !world.denied.contains (access, ref)

def World.perform (world : World) : Actor → String → List String → Option (List String) :=
  fun _ op core => if world.failingOps.contains op then none else some (core ++ [op])

def World.host (world : World) : Host (List String) String String String String :=
  { select := fun _ docs collection => docs.filter fun row => row.ref.collection == collection
  , perform := world.perform
  , allowed := world.allowed }

/-- A scripted plugin decision. `byShown` chooses between two results by the
number of rows the plugin was shown across its declared reads. -/
inductive Decision (V : Type) where
  | refuse (reason : String)
  | proceed (value : V) (effects : List HookEffect)
  | byShown (count : Nat) (matching otherwise : V × List HookEffect)

def Decision.outcome {V : Type} :
    Decision V → List (List HookRow) → Outcome V String String String
  | .refuse reason, _ => .refuse reason
  | .proceed value effects, _ => .proceed value effects
  | .byShown count matching otherwise, rows =>
      let chosen := if (rows.map List.length).sum == count then matching else otherwise
      .proceed chosen.1 chosen.2

/-- A scripted plugin: a manifest, the collections it reads, and its decision. -/
structure Script (V : Type) where
  name : String
  owns : List String
  bound : Nat
  reads : List String := []
  decision : Decision V

def Script.manifest {V : Type} (script : Script V) : Manifest :=
  ⟨script.name, script.owns, script.bound⟩

def Script.plugin {V : Type} (script : Script V) :
    Plugin String Unit V String String String :=
  { manifest := script.manifest
  , hook :=
      { reads := fun _ => script.reads
      , decide := fun _ rows => script.decision.outcome rows } }

def Script.deciding {V : Type} (script : Script V) (decision : Decision V) : Script V :=
  { script with decision := decision }

private def ref (collection doc : String) : Ref String := ⟨collection, doc⟩

private def row (collection doc : String) (revision : Nat) (value : String) : HookRow :=
  ⟨ref collection doc, revision, value⟩

private def create (collection doc value : String) : HookEffect :=
  .write (.create (ref collection doc) value)

private def update (collection doc : String) (expected : Nat) (value : String) : HookEffect :=
  .write (.update (ref collection doc) expected value)

private def delete (collection doc : String) (expected : Nat) : HookEffect :=
  .write (.delete (ref collection doc) expected)

private def coreOp (name : String) : HookEffect := .core name

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def refFields (target : Ref String) : String :=
  "\"collection\":" ++ jsonString target.collection ++ ",\"doc\":" ++ jsonString target.doc

private def rowJson (entry : HookRow) : String :=
  "{" ++ refFields entry.ref ++ ",\"revision\":" ++ toString entry.revision ++
    ",\"value\":" ++ jsonString entry.value ++ "}"

private def accessString : Access → String
  | .create => "create"
  | .update => "update"
  | .delete => "delete"

private def effectJson : HookEffect → String
  | .write (.create target value) =>
      "{\"kind\":\"create\"," ++ refFields target ++ ",\"value\":" ++ jsonString value ++ "}"
  | .write (.update target expected value) =>
      "{\"kind\":\"update\"," ++ refFields target ++ ",\"expected\":" ++ toString expected ++
        ",\"value\":" ++ jsonString value ++ "}"
  | .write (.delete target expected) =>
      "{\"kind\":\"delete\"," ++ refFields target ++ ",\"expected\":" ++ toString expected ++ "}"
  | .core op => "{\"kind\":\"core\",\"op\":" ++ jsonString op ++ "}"

private def effectsJson (effects : List HookEffect) : String :=
  jsonArray (effects.map effectJson)

private def stateJson (state : HookState) : String :=
  "{\"core\":" ++ jsonStringArray state.core ++
    ",\"rows\":" ++ jsonArray (state.docs.map rowJson) ++ "}"

private def worldJson (world : World) : String :=
  "{\"rows\":" ++ jsonArray (world.rows.map rowJson) ++
    ",\"core\":" ++ jsonStringArray world.core ++
    ",\"denied\":" ++ jsonArray (world.denied.map fun entry =>
      "{\"access\":" ++ jsonString (accessString entry.1) ++ "," ++ refFields entry.2 ++ "}") ++
    ",\"failing_ops\":" ++ jsonStringArray world.failingOps ++ "}"

private def manifestJson (manifest : Manifest) : String :=
  "{\"name\":" ++ jsonString manifest.name ++
    ",\"owns\":" ++ jsonStringArray manifest.owns ++
    ",\"bound\":" ++ toString manifest.bound ++ "}"

private def refusalJson : Refusal → String
  | .core => "{\"kind\":\"core\"}"
  | .refused plugin reason =>
      "{\"kind\":\"refused\",\"plugin\":" ++ jsonString plugin ++
        ",\"reason\":" ++ jsonString reason ++ "}"
  | .invalid plugin => "{\"kind\":\"invalid\",\"plugin\":" ++ jsonString plugin ++ "}"
  | .conflict plugin => "{\"kind\":\"conflict\",\"plugin\":" ++ jsonString plugin ++ "}"

private def choiceJson {V : Type} (valueJson : V → String) (choice : V × List HookEffect) :
    String :=
  "{\"value\":" ++ valueJson choice.1 ++ ",\"effects\":" ++ effectsJson choice.2 ++ "}"

private def decisionJson {V : Type} (valueJson : V → String) : Decision V → String
  | .refuse reason => "{\"kind\":\"refuse\",\"reason\":" ++ jsonString reason ++ "}"
  | .proceed value effects =>
      "{\"kind\":\"proceed\",\"value\":" ++ valueJson value ++
        ",\"effects\":" ++ effectsJson effects ++ "}"
  | .byShown count matching otherwise =>
      "{\"kind\":\"by_shown\",\"count\":" ++ toString count ++
        ",\"matching\":" ++ choiceJson valueJson matching ++
        ",\"otherwise\":" ++ choiceJson valueJson otherwise ++ "}"

private def scriptJson {V : Type} (valueJson : V → String) (script : Script V) : String :=
  "{\"name\":" ++ jsonString script.name ++
    ",\"owns\":" ++ jsonStringArray script.owns ++
    ",\"bound\":" ++ toString script.bound ++
    ",\"reads\":" ++ jsonStringArray script.reads ++
    ",\"decision\":" ++ decisionJson valueJson script.decision ++ "}"

private def verdictJson : ClaimVerdict → String
  | .allow => "{\"kind\":\"allow\"}"
  | .leaveQueued => "{\"kind\":\"leave_queued\"}"
  | .retire reason => "{\"kind\":\"retire\",\"reason\":" ++ jsonString reason ++ "}"
  | .reject reason => "{\"kind\":\"reject\",\"reason\":" ++ jsonString reason ++ "}"

/-! Effect validation and application: `Hooks.valid` and `Hooks.commit`. -/

structure EffectCase where
  name : String
  world : World
  manifest : Manifest
  effects : List HookEffect

def EffectCase.accepted (c : EffectCase) : Bool :=
  Hooks.valid c.manifest c.world.allowed caller c.effects

def EffectCase.committed (c : EffectCase) : Option HookState :=
  Hooks.commit c.manifest c.world.allowed c.world.perform caller c.effects c.world.state

private def notes : Manifest := ⟨"notes", ["Note"], 3⟩

private def effectWorld : World :=
  { rows := [row "Note" "n1" 2 "old", row "Other" "o1" 0 "foreign"]
  , denied := [(.create, ref "Note" "n9"), (.delete, ref "Note" "n1")]
  , failingOps := ["explode"] }

def effectCases : List EffectCase :=
  [ { name := "within_bound_applies_in_order", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "fresh", update "Note" "n1" 2 "new"] }
  , { name := "exactly_at_bound", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "a", create "Note" "n3" "b", coreOp "notes.touch"] }
  , { name := "over_bound_is_invalid", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "a", create "Note" "n3" "b", create "Note" "n4" "c",
                  create "Note" "n5" "d"] }
  , { name := "core_operations_count_toward_the_bound", world := effectWorld
    , manifest := ⟨"notes", ["Note"], 0⟩, effects := [coreOp "notes.touch"] }
  , { name := "empty_result_is_valid_and_changes_nothing", world := effectWorld
    , manifest := notes, effects := [] }
  , { name := "write_to_owned_collection", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "owned"] }
  , { name := "write_to_foreign_collection_is_invalid", world := effectWorld, manifest := notes
    , effects := [update "Other" "o1" 0 "taken"] }
  , { name := "one_foreign_write_invalidates_the_result", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "owned", create "Other" "o2" "taken"] }
  , { name := "acp_denied_create_is_invalid", world := effectWorld, manifest := notes
    , effects := [create "Note" "n9" "denied"] }
  , { name := "acp_denies_one_access_not_another", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 2 "allowed"] }
  , { name := "acp_denied_delete_is_invalid", world := effectWorld, manifest := notes
    , effects := [delete "Note" "n1" 2] }
  , { name := "core_operations_only", world := effectWorld, manifest := notes
    , effects := [coreOp "request.supersede", coreOp "fire.mark_applied"] }
  , { name := "failed_core_operation_commits_nothing", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "a", coreOp "explode"] }
  , { name := "create_of_existing_document_misses", world := effectWorld, manifest := notes
    , effects := [create "Note" "n1" "duplicate"] }
  , { name := "update_with_held_revision", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 2 "new"] }
  , { name := "update_with_stale_revision_misses", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 1 "new"] }
  , { name := "update_of_absent_document_misses", world := effectWorld, manifest := notes
    , effects := [update "Note" "n7" 0 "new"] }
  , { name := "delete_with_held_revision"
    , world := { effectWorld with denied := [] }, manifest := notes
    , effects := [delete "Note" "n1" 2] }
  , { name := "delete_with_stale_revision_misses"
    , world := { effectWorld with denied := [] }, manifest := notes
    , effects := [delete "Note" "n1" 3] }
  , { name := "create_then_update_sees_the_created_revision", world := effectWorld
    , manifest := notes, effects := [create "Note" "n2" "a", update "Note" "n2" 0 "b"] }
  , { name := "update_then_create_misses_in_that_order", world := effectWorld
    , manifest := notes, effects := [update "Note" "n2" 0 "b", create "Note" "n2" "a"] }
  , { name := "two_updates_chain_revisions", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 2 "b", update "Note" "n1" 3 "c"] }
  , { name := "delete_then_create_restarts_the_revision"
    , world := { effectWorld with denied := [] }, manifest := notes
    , effects := [delete "Note" "n1" 2, create "Note" "n1" "again"] }
  , { name := "core_operations_apply_in_returned_order", world := effectWorld
    , manifest := notes, effects := [coreOp "second", coreOp "first"] } ]

private def effectCaseJson (c : EffectCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"world\":" ++ worldJson c.world ++
    ",\"manifest\":" ++ manifestJson c.manifest ++
    ",\"effects\":" ++ effectsJson c.effects ++
    ",\"valid\":" ++ boolJson c.accepted ++
    ",\"committed\":" ++ ((c.committed.map stateJson).getD "null") ++ "}"

def effectCasesJson : String := jsonArray (effectCases.map effectCaseJson)

/-! Staging at refusable points: `Hooks.stage` and `Hooks.settle`. -/

private def stageWorld : World :=
  { rows := [row "Note" "n1" 0 "a", row "Tag" "t1" 1 "x"]
  , denied := [(.update, ref "Tag" "locked")]
  , failingOps := ["explode"] }

private def notesPlugin : Script String :=
  { name := "notes", owns := ["Note"], bound := 2, reads := ["Note"]
  , decision := .proceed "notes-ok" [update "Note" "n1" 0 "b", coreOp "notes.op"] }

private def tagsPlugin : Script String :=
  { name := "tags", owns := ["Tag"], bound := 2, reads := ["Tag"]
  , decision := .proceed "tags-ok" [create "Tag" "t2" "y", coreOp "tags.op"] }

private def auditPlugin : Script String :=
  { name := "audit", owns := ["Audit"], bound := 1
  , decision := .proceed "audit-ok" [create "Audit" "a1" "seen"] }

private def tagsWith (decision : Decision String) : Script String :=
  tagsPlugin.deciding decision

/-- The view-rule pair: `writer` creates a Note; `reader` reads Notes and
records how many it was shown. -/
private def writerPlugin : Script String :=
  { name := "writer", owns := ["Note"], bound := 1
  , decision := .proceed "written" [create "Note" "n2" "b"] }

private def readerPlugin : Script String :=
  { name := "reader", owns := ["Mark"], bound := 1, reads := ["Note"]
  , decision := .byShown 1 ("saw-snapshot", [create "Mark" "m1" "one-note"])
      ("saw-staged", [create "Mark" "m1" "two-notes"]) }

structure StageCase where
  name : String
  world : World
  plugins : List (Script String)

def StageCase.result (c : StageCase) : Except Refusal (HookState × List String) :=
  Hooks.stage c.world.host (c.plugins.map Script.plugin) caller () c.world.state

def stageCases : List StageCase :=
  [ { name := "no_plugins_stage_nothing", world := stageWorld, plugins := [] }
  , { name := "all_proceed_apply_in_plugin_order", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin, auditPlugin] }
  , { name := "reversed_order_reverses_values_and_core_operations", world := stageWorld
    , plugins := [auditPlugin, tagsPlugin, notesPlugin] }
  , { name := "refusal_leaves_state_unchanged", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.refuse "tag budget exhausted"), auditPlugin] }
  , { name := "first_failure_in_order_is_reported", world := stageWorld
    , plugins := [tagsWith (.refuse "first"),
        auditPlugin.deciding (.proceed "audit-ok" [create "Note" "n5" "foreign"])] }
  , { name := "foreign_write_is_invalid_and_leaves_state_unchanged", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [create "Note" "n5" "foreign"])] }
  , { name := "acp_denied_write_is_invalid", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [update "Tag" "locked" 0 "z"])] }
  , { name := "over_bound_is_invalid", world := stageWorld
    , plugins := [notesPlugin, auditPlugin.deciding
        (.proceed "audit-ok" [create "Audit" "a1" "x", create "Audit" "a2" "y"])] }
  , { name := "stale_revision_conflicts_and_leaves_state_unchanged", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [update "Tag" "t1" 0 "z"])] }
  , { name := "failed_core_operation_conflicts", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [coreOp "explode"])] }
  , { name := "later_plugin_decides_on_the_shared_snapshot", world := stageWorld
    , plugins := [writerPlugin, readerPlugin] }
  , { name := "earlier_staged_effects_are_not_shown_in_either_order", world := stageWorld
    , plugins := [readerPlugin, writerPlugin] } ]

private def stageResultJson : Except Refusal (HookState × List String) → String
  | .ok (state, values) =>
      "{\"kind\":\"ok\",\"state\":" ++ stateJson state ++
        ",\"values\":" ++ jsonStringArray values ++ "}"
  | .error refusal => "{\"kind\":\"error\",\"refusal\":" ++ refusalJson refusal ++ "}"

private def stageCaseJson (c : StageCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"world\":" ++ worldJson c.world ++
    ",\"plugins\":" ++ jsonArray (c.plugins.map (scriptJson jsonString)) ++
    ",\"result\":" ++ stageResultJson c.result ++
    ",\"settled\":" ++ stateJson (Hooks.settle c.world.state c.result) ++ "}"

def stageCasesJson : String := jsonArray (stageCases.map stageCaseJson)

/-! Must-commit points: `Hooks.mustCommit`, `Hooks.stageDropping`, `Hooks.owed`. -/

structure MustCommitCase where
  name : String
  world : World
  plugins : List (Script String)
  transition : String := "terminal"
  transitionRefused : Bool := false
  readFailed : List String := []

def MustCommitCase.transitionStep (c : MustCommitCase) :
    List String → Option (List String) :=
  fun core => if c.transitionRefused then none else some (core ++ [c.transition])

def MustCommitCase.result (c : MustCommitCase) : Option (HookState × List Bool) :=
  Hooks.mustCommit c.world.host (c.plugins.map Script.plugin) caller c.transitionStep
    (fun _ _ => ()) (fun name => c.readFailed.contains name) c.world.state

def MustCommitCase.owedNames (c : MustCommitCase) : List String :=
  match c.result with
  | none => []
  | some (_, dropped) => Hooks.owed (c.plugins.map Script.plugin) dropped

def mustCommitCases : List MustCommitCase :=
  [ { name := "every_plugin_stages", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin, auditPlugin] }
  , { name := "read_failure_drops_only_that_plugin", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin], readFailed := ["notes"] }
  , { name := "refusing_plugin_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.refuse "cannot record"), auditPlugin] }
  , { name := "invalid_result_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [create "Note" "n5" "foreign"])] }
  , { name := "miss_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin.deciding (.proceed "notes-ok" [update "Note" "n1" 4 "b"]),
                  tagsPlugin] }
  , { name := "failed_core_operation_is_dropped_whole", world := stageWorld
    , plugins := [notesPlugin.deciding
        (.proceed "notes-ok" [update "Note" "n1" 0 "b", coreOp "explode"]), tagsPlugin] }
  , { name := "partial_miss_is_dropped_whole", world := stageWorld
    , plugins := [notesPlugin.deciding
        (.proceed "notes-ok" [update "Note" "n1" 0 "b", update "Note" "n1" 0 "c"]),
                  auditPlugin] }
  , { name := "every_plugin_failing_still_commits_the_transition", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.refuse "cannot record")], readFailed := ["notes"] }
  , { name := "core_refusal_commits_nothing", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin], transitionRefused := true } ]

private def mustCommitCaseJson (c : MustCommitCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"world\":" ++ worldJson c.world ++
    ",\"plugins\":" ++ jsonArray (c.plugins.map (scriptJson jsonString)) ++
    ",\"transition\":" ++ jsonString c.transition ++
    ",\"transition_refused\":" ++ boolJson c.transitionRefused ++
    ",\"read_failed\":" ++ jsonStringArray c.readFailed ++
    ",\"committed\":" ++ ((c.result.map fun result =>
      "{\"state\":" ++ stateJson result.1 ++
        ",\"dropped\":" ++ jsonArray (result.2.map boolJson) ++ "}").getD "null") ++
    ",\"owed\":" ++ jsonStringArray c.owedNames ++ "}"

def mustCommitCasesJson : String := jsonArray (mustCommitCases.map mustCommitCaseJson)

/-! Claim verdicts: `Hooks.combine` and `Hooks.gate`. -/

structure CombineCase where
  name : String
  verdicts : List ClaimVerdict

def combineCases : List CombineCase :=
  [ { name := "no_plugins_allow", verdicts := [] }
  , { name := "all_allow", verdicts := [.allow, .allow, .allow] }
  , { name := "leave_queued_over_allow", verdicts := [.allow, .leaveQueued, .allow] }
  , { name := "retire_over_leave_queued", verdicts := [.leaveQueued, .retire "superseded", .allow] }
  , { name := "reject_over_retire", verdicts := [.retire "superseded", .reject "denied", .leaveQueued] }
  , { name := "reject_over_retire_permuted", verdicts := [.leaveQueued, .reject "denied", .retire "superseded"] }
  , { name := "reject_first_permuted", verdicts := [.reject "denied", .leaveQueued, .retire "superseded"] }
  , { name := "equal_rejects_first_reason_wins", verdicts := [.allow, .reject "first", .reject "second"] }
  , { name := "equal_rejects_swapped_first_reason_wins", verdicts := [.reject "second", .allow, .reject "first"] }
  , { name := "equal_retires_first_reason_wins", verdicts := [.retire "older", .leaveQueued, .retire "newer"] }
  , { name := "single_leave_queued", verdicts := [.leaveQueued] } ]

private def combineCaseJson (c : CombineCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"verdicts\":" ++ jsonArray (c.verdicts.map verdictJson) ++
    ",\"combined\":" ++ verdictJson (Hooks.combine c.verdicts) ++ "}"

def combineCasesJson : String := jsonArray (combineCases.map combineCaseJson)

structure GateCase where
  name : String
  world : World
  plugins : List (Script ClaimVerdict)

def GateCase.result (c : GateCase) : Except Refusal (List ClaimVerdict) :=
  Hooks.gate c.world.host () c.world.state (c.plugins.map Script.plugin)

private def fence : Script ClaimVerdict :=
  { name := "goal-fence", owns := ["GoalFence"], bound := 1, reads := ["Note"]
  , decision := .byShown 1 (.retire "goal superseded", []) (.allow, []) }

private def mailbox : Script ClaimVerdict :=
  { name := "mailbox", owns := ["Mailbox"], bound := 1
  , decision := .proceed .allow [] }

def gateCases : List GateCase :=
  [ { name := "no_plugins_answer_nothing", world := stageWorld, plugins := [] }
  , { name := "every_plugin_allows", world := stageWorld
    , plugins := [mailbox, fence.deciding (.proceed .allow [])] }
  , { name := "verdict_from_shown_rows", world := stageWorld, plugins := [mailbox, fence] }
  , { name := "verdict_from_other_shown_rows"
    , world := { stageWorld with rows := row "Note" "n2" 0 "b" :: stageWorld.rows }
    , plugins := [mailbox, fence] }
  , { name := "mixed_severities_keep_every_verdict", world := stageWorld
    , plugins := [mailbox.deciding (.proceed (.reject "reply rejected") []), fence] }
  , { name := "refusal_is_reported", world := stageWorld
    , plugins := [fence, mailbox.deciding (.refuse "mailbox closed")] }
  , { name := "verdict_with_effects_is_invalid", world := stageWorld
    , plugins := [fence.deciding
        (.proceed (.retire "goal superseded") [create "GoalFence" "f1" "retired"]), mailbox] }
  , { name := "effects_rejected_even_when_allowing", world := stageWorld
    , plugins := [mailbox, fence.deciding (.proceed .allow [coreOp "request.supersede"])] } ]

private def gateResultJson : Except Refusal (List ClaimVerdict) → String
  | .ok verdicts =>
      "{\"kind\":\"ok\",\"verdicts\":" ++ jsonArray (verdicts.map verdictJson) ++
        ",\"combined\":" ++ verdictJson (Hooks.combine verdicts) ++ "}"
  | .error refusal => "{\"kind\":\"error\",\"refusal\":" ++ refusalJson refusal ++ "}"

private def gateCaseJson (c : GateCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"world\":" ++ worldJson c.world ++
    ",\"plugins\":" ++ jsonArray (c.plugins.map (scriptJson verdictJson)) ++
    ",\"result\":" ++ gateResultJson c.result ++ "}"

def gateCasesJson : String := jsonArray (gateCases.map gateCaseJson)

/-! The delivery record: `Hooks.Delivery.publish`, `deliver` and `pending`. -/

inductive DeliveryStep where
  | publish (outcome : String) (keys : List String) (commit : Bool)
  | deliver (key : String) (applied : Bool)

def DeliveryStep.run : DeliveryStep → Record → Record
  | .publish outcome keys commit, record => Delivery.publish record outcome keys commit
  | .deliver key applied, record => Delivery.deliver record key applied

structure DeliveryCase where
  name : String
  initial : Record := ⟨none, [], []⟩
  steps : List DeliveryStep

def DeliveryCase.trace (c : DeliveryCase) : List (DeliveryStep × Record) :=
  (c.steps.foldl (fun (acc : Record × List (DeliveryStep × Record)) step =>
    let next := step.run acc.1
    (next, acc.2 ++ [(step, next)])) (c.initial, [])).2

def deliveryCases : List DeliveryCase :=
  [ { name := "rollback_publishes_nothing"
    , steps := [.publish "completed" ["goal", "mailbox"] false, .deliver "goal" true] }
  , { name := "committed_terminal_owes_each_key"
    , steps := [.publish "completed" ["goal", "mailbox"] true, .deliver "goal" true,
                .deliver "mailbox" true] }
  , { name := "terminal_is_published_once"
    , steps := [.publish "completed" ["goal"] true, .publish "failed" ["mailbox"] true] }
  , { name := "failed_delivery_stays_pending"
    , steps := [.publish "completed" ["goal"] true, .deliver "goal" false,
                .deliver "goal" false, .deliver "goal" true] }
  , { name := "redelivery_is_inert"
    , steps := [.publish "completed" ["goal"] true, .deliver "goal" true,
                .deliver "goal" true, .deliver "goal" false] }
  , { name := "unowed_delivery_is_inert"
    , steps := [.deliver "goal" true, .publish "completed" ["goal"] true] }
  , { name := "duplicate_keys_are_owed_once"
    , steps := [.publish "completed" ["goal", "mailbox", "goal"] true, .deliver "goal" true] }
  , { name := "keys_owed_before_publication_are_merged"
    , initial := ⟨none, ["trigger", "goal"], ["trigger"]⟩
    , steps := [.publish "interrupted" ["goal", "mailbox"] true, .deliver "mailbox" true] } ]

private def recordJson (record : Record) : String :=
  "{\"terminal\":" ++ ((record.terminal.map jsonString).getD "null") ++
    ",\"owed\":" ++ jsonStringArray record.owed ++
    ",\"delivered\":" ++ jsonStringArray record.delivered ++ "}"

private def deliveryStepJson : DeliveryStep → String
  | .publish outcome keys commit =>
      "{\"kind\":\"publish\",\"outcome\":" ++ jsonString outcome ++
        ",\"keys\":" ++ jsonStringArray keys ++ ",\"commit\":" ++ boolJson commit ++ "}"
  | .deliver key applied =>
      "{\"kind\":\"deliver\",\"key\":" ++ jsonString key ++
        ",\"applied\":" ++ boolJson applied ++ "}"

private def deliveryCaseJson (c : DeliveryCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"initial\":" ++ recordJson c.initial ++
    ",\"trace\":" ++ jsonArray (c.trace.map fun entry =>
      "{\"step\":" ++ deliveryStepJson entry.1 ++
        ",\"state\":" ++ recordJson entry.2 ++
        ",\"pending\":" ++ jsonStringArray (Delivery.pending entry.2) ++ "}") ++ "}"

def deliveryCasesJson : String := jsonArray (deliveryCases.map deliveryCaseJson)

end Conformance.HooksContracts
