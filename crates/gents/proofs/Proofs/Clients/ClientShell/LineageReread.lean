/-! When the desktop reads the open session's lineage again (`hooks/provenance.ts`).

The lineage of a session (its starter, the sessions it started, sent to or
heard from, the senders of its turns, the sessions whose requests its calls
caused, and the related sessions it reported) can move only when one of those
sessions moves, or a session newly names it as its starter. A read records
each listed session's moment (its turn state and update time). Another read is
asked only when a session that can move the lineage has a different moment
than the last read saw, or the scope, the open session's rows or the remote
lineage version changed. -/

namespace LineageReread

/-- A session a lineage names, on a node. -/
structure Link where
  node : String
  session : String
  deriving DecidableEq, Repr

/-- The links of a read lineage, by what each one is. -/
structure Lineage where
  startedBy : Option Link
  started : List Link
  sent : List Link
  received : List Link
  senders : List Link
  calls : List Link
  related : List Link
  deriving DecidableEq, Repr

def Lineage.links (lineage : Lineage) : List Link :=
  lineage.startedBy.toList ++ lineage.started ++ lineage.sent ++ lineage.received ++
    lineage.senders ++ lineage.calls ++ lineage.related

/-- A session of the open session's node as the session list shows it. -/
structure Listed where
  session : String
  startedBy : Option Link
  moment : String
  deriving DecidableEq, Repr

/-- Whether a listed session of `node` can move the lineage of `opened`: it is
the open session, the lineage names it on that node, or it names the open
session as its starter. -/
def moves (node opened : String) (lineage : Option Lineage) (listed : Listed) : Bool :=
  listed.session == opened ||
    listed.startedBy == some ⟨node, opened⟩ ||
    lineage.any fun read => read.links.any fun link =>
      link.node == node && link.session == listed.session

def lineageSessions (node opened : String) (lineage : Option Lineage)
    (sessions : List Listed) : List Listed :=
  sessions.filter (moves node opened lineage)

/-- A listed session's moment when the last read was asked. -/
structure Moment where
  session : String
  moment : String
  deriving DecidableEq, Repr

/-- What the last read was asked against. `cues` is the scope and the open
session's rows; `version` the remote lineage's. -/
structure Asked where
  cues : String
  version : Option Nat
  moments : List Moment
  deriving DecidableEq, Repr

def seen (asked : Asked) (listed : Listed) : Bool :=
  (asked.moments.find? (·.session == listed.session)).map (·.moment) == some listed.moment

/-- Whether the desktop reads the lineage again. -/
def asksAgain (asked : Option Asked) (cues : String) (version : Option Nat)
    (node opened : String) (lineage : Option Lineage) (sessions : List Listed) : Bool :=
  match asked with
  | none => true
  | some asked =>
    asked.cues != cues || asked.version != version ||
      (lineageSessions node opened lineage sessions).any fun listed => !seen asked listed

/-- A change to sessions that cannot move the lineage never asks for a read:
with the scope, rows and version as last asked, and every session that can
move it as the last read saw it, nothing is read, whatever else changed. -/
theorem an_unrelated_change_never_reads (asked : Asked) (node opened : String)
    (lineage : Option Lineage) (sessions : List Listed)
    (unmoved : ∀ listed ∈ sessions, moves node opened lineage listed = true →
      seen asked listed = true) :
    asksAgain (some asked) asked.cues asked.version node opened lineage sessions = false := by
  simp only [asksAgain, bne_self_eq_false, Bool.false_or, List.any_eq_false,
    lineageSessions, List.mem_filter, Bool.not_eq_true']
  intro listed ⟨member, movable⟩
  simpa using unmoved listed member movable

/-- A change to any session that can move the lineage always asks for a read. -/
theorem a_named_change_always_reads (asked : Option Asked) (cues : String)
    (version : Option Nat) (node opened : String) (lineage : Option Lineage)
    (sessions : List Listed) (listed : Listed) (member : listed ∈ sessions)
    (movable : moves node opened lineage listed = true)
    (changed : ∀ a, asked = some a → seen a listed = false) :
    asksAgain asked cues version node opened lineage sessions = true := by
  cases asked with
  | none => rfl
  | some a =>
    simp only [asksAgain, Bool.or_eq_true, bne_iff_ne, ne_eq, List.any_eq_true,
      lineageSessions, List.mem_filter, Bool.not_eq_true']
    exact Or.inr ⟨listed, ⟨member, movable⟩, changed a rfl⟩

end LineageReread
