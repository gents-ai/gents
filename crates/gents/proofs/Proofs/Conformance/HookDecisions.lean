import Proofs.Session.Hooks.Supersede
import Proofs.Session.Hooks.History
import Proofs.Automation.Hooks.GoalHalt
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.HookDecisions
open Hooks
open Conformance.Contracts (jsonString jsonArray jsonStringArray)

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def optionJson (value : Option String) : String := (value.map jsonString).getD "null"

/-! `request.pending.supersede`: membership and the retirement reason of each
retired continuation (`Hooks.Selector.stale`). -/

structure SupersedeCase where
  name : String
  selector : Selector
  rows : List PendingRow
  parents : List ParentRow

def selectorB (next : Option String) : Selector :=
  { goal := "goal-b", root := "root-b", successor := "request-root-b", successorDoc := "root-b"
  , reason := "superseded by a newer Task Goal assignment", next := next }

def showGoal : Option String := some "gents automation goal show --session session-1"

/-- A pending continuation of `goal-a`'s assignment opened at `root-a`,
signed, whose parent is `parent`. -/
def continuation (doc parent : String) : PendingRow :=
  { doc := doc, request := "request-" ++ doc, pending := true, goalContinuation := true
  , trigger := some "goal-a", reference := some ("goal-a", some "root-a"), signed := true
  , parent := some parent }

/-- A visible parent of the same session, signed, itself a continuation of
`goal-a` in the assignment opened at `root-a`. -/
def parentRow (doc : String) : ParentRow :=
  { doc := doc, inSession := true, signed := true, goal := some "goal-a"
  , reference := some ("goal-a", some "root-a") }

def supersedeCases : List SupersedeCase :=
  [ { name := "every_link_is_classified", selector := selectorB showGoal
    , rows :=
        [ continuation "ordinary" "p-ordinary"
        , { continuation "unsigned" "p-ordinary" with signed := false }
        , { continuation "other-goal" "p-ordinary" with reference := some ("goal-c", some "root-a") }
        , { continuation "orphan" "p-ordinary" with parent := none }
        , continuation "absent" "p-missing"
        , continuation "foreign" "p-foreign"
        , continuation "unsigned-parent" "p-unsigned"
        , continuation "diverged-root" "p-other-root"
        , continuation "diverged-goal" "p-other-goal"
        , continuation "from-root" "root-a"
        , { continuation "legacy" "p-legacy" with reference := none } ]
    , parents :=
        [ parentRow "p-ordinary"
        , { parentRow "p-foreign" with inSession := false }
        , { parentRow "p-unsigned" with signed := false }
        , { parentRow "p-other-root" with reference := some ("goal-a", some "root-x") }
        , { parentRow "p-other-goal" with goal := some "goal-c" }
        , { doc := "root-a", inSession := true, signed := true, goal := none
          , reference := some ("goal-a", none) }
        , { doc := "p-legacy", inSession := true, signed := true, goal := some "goal-a"
          , reference := none } ] }
  , { name := "members_idle_and_other_rows_are_kept", selector := selectorB showGoal
    , rows :=
        [ { continuation "member" "root-b" with
            trigger := some "goal-b", reference := some ("goal-b", some "root-b") }
        , { continuation "root-b" "p-ordinary" with signed := false }
        , { continuation "running" "p-ordinary" with pending := false, signed := false }
        , { continuation "user" "p-ordinary" with goalContinuation := false, signed := false }
        , { continuation "stale" "p-missing" with signed := false } ]
    , parents := [parentRow "p-ordinary"] }
  , { name := "a_broken_ancestry_without_a_next_call", selector := selectorB none
    , rows := [continuation "absent" "p-missing", continuation "ordinary" "p-ordinary"]
    , parents := [parentRow "p-ordinary"] } ]

private def referenceJson : Option (String × Option String) → String
  | none => "null"
  | some (goal, root) => "{\"goal\":" ++ jsonString goal ++ ",\"root\":" ++ optionJson root ++ "}"

private def linkJson : BrokenLink → String
  | .unsigned => "unsigned"
  | .otherGoal => "other_goal"
  | .noParent => "no_parent"
  | .parentAbsent => "parent_absent"
  | .parentOutsideSession => "parent_outside_session"
  | .parentUnsigned => "parent_unsigned"
  | .diverges => "diverges"

private def ancestryJson : Ancestry → String
  | .ordinary => "{\"kind\":\"ordinary\"}"
  | .broken link => "{\"kind\":\"broken\",\"link\":" ++ jsonString (linkJson link) ++ "}"

private def pendingRowJson (row : PendingRow) : String :=
  "{\"doc\":" ++ jsonString row.doc ++ ",\"request\":" ++ jsonString row.request ++
    ",\"pending\":" ++ boolJson row.pending ++
    ",\"goal_continuation\":" ++ boolJson row.goalContinuation ++
    ",\"trigger\":" ++ optionJson row.trigger ++ ",\"reference\":" ++ referenceJson row.reference ++
    ",\"signed\":" ++ boolJson row.signed ++ ",\"parent\":" ++ optionJson row.parent ++ "}"

private def parentRowJson (row : ParentRow) : String :=
  "{\"doc\":" ++ jsonString row.doc ++ ",\"in_session\":" ++ boolJson row.inSession ++
    ",\"signed\":" ++ boolJson row.signed ++ ",\"goal\":" ++ optionJson row.goal ++
    ",\"reference\":" ++ referenceJson row.reference ++ "}"

private def selectorJson (selector : Selector) : String :=
  "{\"goal\":" ++ jsonString selector.goal ++ ",\"root\":" ++ jsonString selector.root ++
    ",\"successor\":" ++ jsonString selector.successor ++
    ",\"successor_doc\":" ++ jsonString selector.successorDoc ++
    ",\"reason\":" ++ jsonString selector.reason ++ ",\"next\":" ++ optionJson selector.next ++ "}"

private def staleJson (stale : StaleRequest) : String :=
  "{\"doc\":" ++ jsonString stale.doc ++ ",\"request\":" ++ jsonString stale.request ++
    ",\"successor\":" ++ jsonString stale.successor ++
    ",\"successor_doc\":" ++ jsonString stale.successorDoc ++
    ",\"reason\":" ++ jsonString stale.reason ++ ",\"ancestry\":" ++ ancestryJson stale.ancestry ++ "}"

private def supersedeCaseJson (c : SupersedeCase) : String :=
  "{\"name\":" ++ jsonString c.name ++ ",\"selector\":" ++ selectorJson c.selector ++
    ",\"rows\":" ++ jsonArray (c.rows.map pendingRowJson) ++
    ",\"parents\":" ++ jsonArray (c.parents.map parentRowJson) ++
    ",\"stale\":" ++ jsonArray ((c.selector.stale c.rows c.parents).map staleJson) ++ "}"

def supersedeCasesJson : String := jsonArray (supersedeCases.map supersedeCaseJson)

/-! `control.session.halt`, the Goals listener (`Hooks.GoalHalt.verdict`). A
time is its RFC 3339 text with the instant (nanoseconds since the epoch) the
model compares; `none` where the text does not parse. -/

structure Stamp where
  text : String
  instant : Option GoalHalt.Instant

structure GoalHaltCase where
  name : String
  status : Option Goals.Status
  updated : Option Stamp
  halt : Stamp

def nine : Stamp := ⟨"2026-11-11T09:00:00Z", some 1794387600000000000⟩
def nineAtPlusTwo : Stamp := ⟨"2026-11-11T11:00:00+02:00", some 1794387600000000000⟩
def eightUtc : Stamp := ⟨"2026-11-11T08:00:00Z", some 1794384000000000000⟩
def eightAtPlusTwo : Stamp := ⟨"2026-11-11T10:00:00+02:00", some 1794384000000000000⟩
def halfPastNine : Stamp := ⟨"2026-11-11T09:30:00Z", some 1794389400000000000⟩
def ninePlusHalfSecond : Stamp := ⟨"2026-11-11T09:00:00.5Z", some 1794387600500000000⟩
def unparseable : Stamp := ⟨"yesterday", none⟩

def goalHaltCases : List GoalHaltCase :=
  [ { name := "no_goal_is_left_alone", status := none, updated := none, halt := nine }
  , { name := "an_active_goal_is_paused", status := some .active, updated := some eightUtc
    , halt := nine }
  , { name := "a_budget_limited_goal_is_paused", status := some .budgetLimited
    , updated := some eightUtc, halt := nine }
  , { name := "a_goal_without_a_change_time_is_paused", status := some .active, updated := none
    , halt := nine }
  , { name := "equal_times_pause", status := some .active, updated := some nineAtPlusTwo
    , halt := nine }
  , { name := "a_later_change_is_kept", status := some .active, updated := some halfPastNine
    , halt := nine }
  , { name := "a_later_fraction_of_a_second_is_kept", status := some .active
    , updated := some ninePlusHalfSecond, halt := nine }
  , { name := "an_earlier_instant_with_later_text_pauses", status := some .active
    , updated := some eightAtPlusTwo, halt := nine }
  , { name := "a_later_instant_with_earlier_text_is_kept", status := some .active
    , updated := some halfPastNine, halt := ⟨"2026-11-11T10:00:00+02:00", some 1794384000000000000⟩ }
  , { name := "an_unparseable_change_time_pauses", status := some .active
    , updated := some unparseable, halt := nine }
  , { name := "an_unparseable_halt_time_is_refused", status := some .active
    , updated := some eightUtc, halt := unparseable }
  , { name := "a_paused_goal_is_left_alone", status := some .paused, updated := some eightUtc
    , halt := nine }
  , { name := "an_unparseable_halt_of_a_paused_goal_is_inert", status := some .paused
    , updated := none, halt := unparseable }
  , { name := "a_complete_goal_is_left_alone", status := some .complete, updated := none
    , halt := nine }
  , { name := "a_blocked_goal_is_left_alone", status := some .blocked, updated := none
    , halt := nine } ]

structure StampWire where
  text : String
  instant : Option Int
  deriving Lean.ToJson

def StampWire.ofStamp (stamp : Stamp) : StampWire := { text := stamp.text, instant := stamp.instant }

structure GoalHaltInput where
  name : String
  status : Option String
  updated : Option StampWire
  halt : StampWire
  deriving Lean.ToJson

inductive HaltVerdictKind where
  | inactive
  | changedAfter
  | unreadableHalt
  | pause
  deriving Lean.ToJson

structure PausedGoalWire where
  status : String
  blockedAudits : Nat
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving Lean.ToJson

structure GoalHaltOutput where
  kind : HaltVerdictKind
  paused : Option PausedGoalWire
  deriving Lean.ToJson

def GoalHaltOutput.ofVerdict : GoalHalt.Verdict → GoalHaltOutput
  | .inactive => { kind := .inactive, paused := none }
  | .changedAfter => { kind := .changedAfter, paused := none }
  | .unreadableHalt => { kind := .unreadableHalt, paused := none }
  | .pause paused =>
      { kind := .pause
        paused := some { status := paused.status.toDefraDB, blockedAudits := paused.blockedAudits
                         wrapupRequested := paused.wrapupRequested
                         wrapupCompleted := paused.wrapupCompleted } }

@[conformance_family]
def goalHalt : Family GoalHaltInput GoalHaltOutput where
  inputs := goalHaltCases.map fun c =>
    { name := c.name, status := c.status.map Goals.Status.toDefraDB
      updated := c.updated.map StampWire.ofStamp, halt := StampWire.ofStamp c.halt }
  model := fun i =>
    let goal := (i.status.bind Goals.Status.fromDefraDB?).map fun status =>
      (⟨⟨status, 0, false, false⟩, i.updated.bind (·.instant)⟩ : GoalHalt.Goal)
    GoalHaltOutput.ofVerdict (GoalHalt.verdict goal i.halt.instant)

/-! The bound on a declared history read (`Hooks.historyPermitted`). -/

structure HistoryCase where
  name : String
  owns : List String
  history : List String
  named : List (Ref String)
  read : HistoryRead String

def historyCases : List HistoryCase :=
  [ { name := "an_owned_declared_collection", owns := ["Goal"], history := ["Goal"], named := []
    , read := ⟨"Goal", none⟩ }
  , { name := "an_owned_declared_document", owns := ["Goal"], history := ["Goal"], named := []
    , read := ⟨"Goal", some "goal-1"⟩ }
  , { name := "an_owned_undeclared_collection", owns := ["Goal"], history := [], named := []
    , read := ⟨"Goal", none⟩ }
  , { name := "a_core_collection", owns := ["Goal"], history := ["AgentRequest"], named := []
    , read := ⟨"AgentRequest", none⟩ }
  , { name := "a_document_the_event_names", owns := ["Goal"], history := ["AgentRequest"]
    , named := [⟨"AgentRequest", "request-1"⟩], read := ⟨"AgentRequest", some "request-1"⟩ }
  , { name := "a_document_the_event_does_not_name", owns := ["Goal"]
    , history := ["AgentRequest"], named := [⟨"AgentRequest", "request-1"⟩]
    , read := ⟨"AgentRequest", some "request-2"⟩ }
  , { name := "every_document_of_a_named_collection", owns := ["Goal"]
    , history := ["AgentRequest"], named := [⟨"AgentRequest", "request-1"⟩]
    , read := ⟨"AgentRequest", none⟩ }
  , { name := "a_named_document_undeclared", owns := ["Goal"], history := ["Goal"]
    , named := [⟨"AgentRequest", "request-1"⟩], read := ⟨"AgentRequest", some "request-1"⟩ }
  , { name := "a_named_document_of_another_collection", owns := ["Goal"]
    , history := ["AgentSession"], named := [⟨"AgentRequest", "request-1"⟩]
    , read := ⟨"AgentSession", some "request-1"⟩ } ]

structure HistoryRefWire where
  collection : String
  doc : String
  deriving Lean.ToJson

structure HistoryReadWire where
  collection : String
  doc : Option String
  deriving Lean.ToJson

structure HistoryInput where
  name : String
  owns : List String
  history : List String
  named : List HistoryRefWire
  read : HistoryReadWire
  deriving Lean.ToJson

structure HistoryOutput where
  permitted : Bool
  deriving Lean.ToJson

@[conformance_family]
def history : Family HistoryInput HistoryOutput where
  inputs := historyCases.map fun c =>
    { name := c.name, owns := c.owns, history := c.history
      named := c.named.map fun ref => { collection := ref.collection, doc := ref.doc }
      read := { collection := c.read.collection, doc := c.read.doc } }
  model := fun i =>
    { permitted := historyPermitted ⟨"plugin", i.owns, 0⟩ i.history
        (i.named.map fun ref => (⟨ref.collection, ref.doc⟩ : Ref String))
        (⟨i.read.collection, i.read.doc⟩ : HistoryRead String) }

end Conformance.HookDecisions
