import Proofs.ClientShell.SessionLoad

namespace Conformance.ClientSessionLoad
open _root_.ClientSessionLoad

private def q (s : String) : String := "\"" ++ s ++ "\""

private def optStr : Option String → String
  | none => "null"
  | some s => q s

private def optBool : Option Bool → String
  | none => "null"
  | some b => toString b

def phaseName : Phase → String
  | .idle => "idle" | .loading => "loading" | .pending => "pending"
  | .delivering => "delivering" | .notConnected => "notConnected"
  | .accepted => "accepted" | .loaded => "loaded" | .failed => "failed"

def layerName : Layer → String
  | .localDatabase => "localDatabase" | .p2p => "p2p" | .sessionSync => "sessionSync"
  | .sync => "sync" | .runtime => "runtime" | .inference => "inference"

def actionName : Option Action → String
  | none => "null"
  | some .retryLocal => q "retryLocal"
  | some .reloadSession => q "reloadSession"
  | some .configureInference => q "configureInference"

def statusPhaseName : StatusPhase → String
  | .loading => "loading" | .blocked => "blocked" | .failed => "failed"

def operationalLayerName : OperationalLayer → String
  | .client => "client" | .selection => "selection" | .sync => "sync" | .p2p => "p2p"
  | .route => "route" | .runtime => "runtime" | .inference => "inference"
  | .reconcile => "reconcile"

def operationalKindName : OperationalKind → String
  | .ready => "ready" | .working => "working" | .waiting => "waiting"
  | .syncing => "syncing" | .blocked => "blocked"

def statusJson : Option Status → String
  | none => "null"
  | some s =>
      "{\"layer\":" ++ q (layerName s.layer) ++ ",\"phase\":" ++ q (statusPhaseName s.phase) ++
      ",\"title\":" ++ q s.title ++ ",\"detail\":" ++ q s.detail ++
      ",\"action\":" ++ actionName s.action ++ "}"

def blockerJson : Option Blocker → String
  | none => "null"
  | some b =>
      "{\"layer\":" ++ q (operationalLayerName b.layer) ++
      ",\"kind\":" ++ q (operationalKindName b.kind) ++ ",\"label\":" ++ q b.label ++
      ",\"detail\":" ++ q b.detail ++ ",\"configureInference\":" ++
      toString b.configureInference ++ "}"

def phases : List Phase :=
  [.idle, .loading, .pending, .delivering, .notConnected, .accepted, .loaded, .failed]

def bools : List Bool := [false, true]

def inputJson (i : Input) : String :=
  "{\"selected\":" ++ toString i.selected ++ ",\"sessionMatches\":" ++
  toString i.sessionMatches ++ ",\"loadMatches\":" ++ toString i.loadMatches ++
  ",\"phase\":" ++ q (phaseName i.phase) ++ ",\"error\":" ++ optStr i.error ++
  ",\"found\":" ++ optBool i.found ++ ",\"reread\":" ++ toString i.reread ++
  ",\"blocker\":" ++ blockerJson i.blocker ++ ",\"expected\":" ++ statusJson (project i) ++ "}"

def sampleBlocker : Blocker :=
  { layer := .runtime, kind := .blocked, label := "Runtime offline",
    detail := "The runtime is not answering.", configureInference := false }

def statusRows : List String :=
  bools.flatMap fun selected =>
    bools.flatMap fun sessionMatches =>
      bools.flatMap fun loadMatches =>
        phases.flatMap fun phase =>
          [none, some "read failed"].flatMap fun error =>
            [none, some false, some true].flatMap fun found =>
              bools.flatMap fun reread =>
                [none, some sampleBlocker].map fun blocker =>
                  inputJson { selected, sessionMatches, loadMatches, phase, error, found,
                              reread, blocker }

def operationalLayers : List OperationalLayer :=
  [.client, .selection, .sync, .p2p, .route, .runtime, .inference, .reconcile]

def operationalKinds : List OperationalKind := [.ready, .working, .waiting, .syncing, .blocked]

/-- Every operational blocker shape, placed over a shown session. -/
def operationalRows : List String :=
  operationalLayers.flatMap fun layer =>
    operationalKinds.flatMap fun kind =>
      bools.map fun configureInference =>
        inputJson { selected := true, sessionMatches := true, loadMatches := true,
                    phase := .loaded, error := none, found := some true, reread := false,
                    blocker := some { layer, kind, label := "Blocked here",
                                      detail := "Because of this.", configureInference } }

def nodeName : Option Nat → String
  | none => "null"
  | some n => q ("did:key:node-" ++ toString n)

def matchRows : List String :=
  let nodes : List (Option Nat) := [none, some 0, some 1]
  bools.flatMap fun sameSession =>
    nodes.flatMap fun selectedNode =>
      nodes.map fun targetNode =>
        "{\"sameSession\":" ++ toString sameSession ++ ",\"selectedNode\":" ++
        nodeName selectedNode ++ ",\"targetNode\":" ++ nodeName targetNode ++
        ",\"expected\":" ++
        toString (matchesSelection 0 (if sameSession then 0 else 1) selectedNode targetNode) ++ "}"

def rereadRows : List String :=
  (List.range (rereadBudget + 2)).map fun used =>
    "{\"used\":" ++ toString used ++ ",\"armed\":" ++
    toString (rereadArmed (rereadBudget - used)) ++ "}"

def casesJson : String :=
  "{\"statuses\":[" ++ String.intercalate "," statusRows ++
  "],\"operational\":[" ++ String.intercalate "," operationalRows ++
  "],\"matches\":[" ++ String.intercalate "," matchRows ++
  "],\"rereadBudget\":" ++ toString rereadBudget ++
  ",\"rereads\":[" ++ String.intercalate "," rereadRows ++ "]}"

end Conformance.ClientSessionLoad

def main : IO Unit := IO.println Conformance.ClientSessionLoad.casesJson
