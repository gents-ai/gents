import Proofs.Clients.ClientShell.LineageReread
import Proofs.Conformance.Family

namespace LineageReread.Contracts

open LineageReread Lean

deriving instance ToJson for Link, Lineage, Listed, Moment, Asked

structure Input where
  name : String
  node : String
  opened : String
  lineage : Option Lineage
  sessions : List Listed
  asked : Option Asked
  cues : String
  version : Option Nat
  deriving ToJson

structure Decision where
  movable : List String
  asksAgain : Bool
  deriving ToJson

private def node : String := "did:key:node"
private def other : String := "did:key:other"
private def at' (session : String) : Link := ⟨node, session⟩
private def none' : Lineage := ⟨none, [], [], [], [], [], []⟩
private def listed (session moment : String) (startedBy : Option Link := none) : Listed :=
  ⟨session, startedBy, moment⟩
private def asked (moments : List (String × String)) : Asked :=
  ⟨"scope", some 1, moments.map fun (session, moment) => ⟨session, moment⟩⟩
private def case (name : String) (lineage : Option Lineage) (sessions : List Listed)
    (moments : Option (List (String × String))) (cues : String := "scope")
    (version : Option Nat := some 1) : Input :=
  { name, node, opened := "open", lineage, sessions, asked := moments.map asked, cues, version }

/-- The sessions as the last read saw them: every one idle. -/
private def before : List (String × String) :=
  [("open", "idle"), ("named", "idle"), ("unrelated", "idle")]

private def lineageNaming (field : String) : Lineage :=
  let link := [at' "named"]
  match field with
  | "startedBy" => { none' with startedBy := some (at' "named") }
  | "started" => { none' with started := link }
  | "sent" => { none' with sent := link }
  | "received" => { none' with received := link }
  | "senders" => { none' with senders := link }
  | "calls" => { none' with calls := link }
  | _ => { none' with related := link }

private def fields : List String :=
  ["startedBy", "started", "sent", "received", "senders", "calls", "related"]

private def cases : List Input :=
  [ case "the_first_read_is_asked" (some none') [listed "open" "idle"] none
  , case "nothing_moved" (some (lineageNaming "started"))
      [listed "open" "idle", listed "named" "idle", listed "unrelated" "idle"] (some before)
  , case "an_unrelated_session_moved" (some (lineageNaming "started"))
      [listed "open" "idle", listed "named" "idle", listed "unrelated" "running"] (some before)
  , case "the_open_session_moved" (some none')
      [listed "open" "running", listed "unrelated" "idle"] (some before)
  , case "a_new_child_appeared" (some none')
      [listed "open" "idle", listed "child" "running" (some (at' "open"))] (some before)
  , case "a_session_named_on_another_node_does_not_count" (some { none' with started := [⟨other, "unrelated"⟩] })
      [listed "open" "idle", listed "unrelated" "running"] (some before)
  , case "a_lineage_not_yet_read_names_only_the_open_session" none
      [listed "open" "idle", listed "named" "running"] (some before)
  , case "a_lineage_that_names_a_listed_session_is_not_a_change" (some (lineageNaming "calls"))
      [listed "open" "idle", listed "named" "idle"] (some before)
  , case "the_rows_moved" (some none') [listed "open" "idle"] (some before) (cues := "scope-rows-2")
  , case "the_remote_version_moved" (some none') [listed "open" "idle"] (some before)
      (version := some 2) ] ++
  fields.map fun field =>
    case s!"a_session_its_lineage_names_as_{field}_moved" (some (lineageNaming field))
      [listed "open" "idle", listed "named" "running", listed "unrelated" "running"] (some before)

@[conformance_family]
def decision : Conformance.Family Input Decision where
  inputs := cases
  model := fun i =>
    { movable := (lineageSessions i.node i.opened i.lineage i.sessions).map (·.session)
      asksAgain := asksAgain i.asked i.cues i.version i.node i.opened i.lineage i.sessions }

end LineageReread.Contracts
