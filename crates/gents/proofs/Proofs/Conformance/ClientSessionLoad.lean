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

/-! ## Follow-up traces

The follow-up machine is replayed against the production owners. A trace is a
sequence of the user's, the runtime's and the client's actions; between them
the observer's drain loop starts any owed read, one at a time (`settle`). Each
action is a `Step` (`apply_reachable`), so every observed state is `Reachable`
and `phase_has_exit` covers it. The retry action is taken only while the
projected status offers it, which is when no read runs. -/

structure Driver where
  s : State
  /-- the drain loop is running a read of its own -/
  drainBusy : Bool

inductive Reply where
  | found | missing | pending | delivering | notConnected | accepted | failed | mismatch
  deriving DecidableEq, Repr

def Reply.outcome : Reply → Outcome
  | .found => .found
  | .missing => .missing
  | .pending => .waiting .pending rfl
  | .delivering => .waiting .delivering rfl
  | .notConnected => .waiting .notConnected rfl
  | .accepted => .waiting .accepted rfl
  | .failed => .failed
  | .mismatch => .mismatch

def Reply.name : Reply → String
  | .found => "found" | .missing => "missing" | .pending => "pending"
  | .delivering => "delivering" | .notConnected => "notConnected"
  | .accepted => "accepted" | .failed => "failed" | .mismatch => "mismatch"

def replies : List Reply :=
  [.found, .missing, .pending, .delivering, .notConnected, .accepted, .failed, .mismatch]

inductive Act where
  | select (drop : Bool)
  | notice
  | complete (r : Reply)
  | fire
  | retry
  | drop
  | stop
  | restart
  deriving DecidableEq, Repr, Inhabited

def settle (d : Driver) : Driver :=
  if d.s.running && d.s.queued && !d.drainBusy then { s := beginRead d.s, drainBusy := true }
  else d

def retryOffered (s : State) : Bool :=
  (project (input s none)).bind (·.action) == some .retryLocal

def other (n : Nat) : Nat := if n == 0 then 1 else 0

def selectState (s : State) (drop : Bool) : State :=
  { s with selected := other s.selected, queued := s.queued || s.running,
           fresh := s.fresh || s.running, timer := false,
           shown := if drop then none else s.shown }

def apply (d : Driver) : Act → Option Driver
  | .select drop => some (settle { d with s := selectState d.s drop })
  | .notice =>
      if d.s.running then some (settle { d with s := { d.s with queued := true, fresh := true } })
      else none
  | .complete r =>
      match d.s.reading with
      | some t => some (settle { s := finish d.s t r.outcome, drainBusy := false })
      | none => none
  | .fire =>
      if d.s.timer then some (settle { d with s := { d.s with timer := false, queued := true } })
      else none
  | .retry =>
      if retryOffered d.s then some (settle { d with s := { launch d.s with budget := rereadBudget } })
      else none
  | .drop => some (settle { d with s := { d.s with shown := none } })
  | .stop => if d.s.running then some { s := stopObserver d.s, drainBusy := false } else none
  | .restart =>
      if !d.s.running then
        some (settle { d with s := { d.s with running := true, queued := true, fresh := true } })
      else none

theorem settle_reachable (d : Driver) (h : Reachable d.s) : Reachable (settle d).s := by
  unfold settle
  split
  · rename_i hc
    simp only [Bool.and_eq_true, Bool.not_eq_true'] at hc
    exact Reachable.step h (Step.begin _ hc.1.2)
  · exact h

theorem other_ne (n : Nat) : other n ≠ n := by
  unfold other
  split <;> rename_i h <;> simp only [beq_iff_eq] at h <;> omega

theorem apply_reachable (d d' : Driver) (a : Act) (hs : Reachable d.s)
    (h : apply d a = some d') : Reachable d'.s := by
  cases a with
  | select drop =>
      simp only [apply, Option.some.injEq] at h; subst h
      exact settle_reachable _ (Reachable.step hs (Step.select _ _ drop (other_ne _)))
  | notice =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        rename_i hr; exact settle_reachable _ (Reachable.step hs (Step.notice _ hr))
      · cases h
  | complete r =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        rename_i t ht
        exact settle_reachable _ (Reachable.step hs (Step.complete _ t r.outcome ht))
      · cases h
  | fire =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        rename_i ht; exact settle_reachable _ (Reachable.step hs (Step.fire _ ht))
      · cases h
  | retry =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        exact settle_reachable _ (Reachable.step hs (Step.retry _))
      · cases h
  | drop =>
      simp only [apply, Option.some.injEq] at h; subst h
      exact settle_reachable _ (Reachable.step hs (Step.drop _))
  | stop =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        rename_i hr; exact Reachable.step hs (Step.stop _ hr)
      · cases h
  | restart =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        rename_i hr
        simp only [Bool.not_eq_true'] at hr
        exact settle_reachable _ (Reachable.step hs (Step.restart _ hr))
      · cases h

def start : Driver := settle { s := initial 0, drainBusy := false }

theorem start_reachable : Reachable start.s := settle_reachable _ (Reachable.init 0)

def allActs : List Act :=
  [.select false, .select true, .drop, .notice, .stop, .restart, .fire, .retry] ++
    replies.map .complete

def enabled (d : Driver) : List Act := allActs.filter fun a => (apply d a).isSome

def actJson : Act → String
  | .select drop => "{\"kind\":\"select\",\"drop\":" ++ toString drop ++ "}"
  | .notice => "{\"kind\":\"notice\"}"
  | .complete r => "{\"kind\":\"complete\",\"reply\":" ++ q r.name ++ "}"
  | .fire => "{\"kind\":\"fire\"}"
  | .retry => "{\"kind\":\"retry\"}"
  | .drop => "{\"kind\":\"drop\"}"
  | .stop => "{\"kind\":\"stop\"}"
  | .restart => "{\"kind\":\"restart\"}"

private def optNat : Option Nat → String
  | none => "null"
  | some n => toString n

/-- What a trace compares after each action. The status detail carries error
text the model abstracts, so the projection rows above check it instead. -/
def obsJson (d : Driver) : String :=
  let s := d.s
  let status := match project (input s none) with
    | none => "null"
    | some st =>
        "{\"layer\":" ++ q (layerName st.layer) ++ ",\"phase\":" ++
        q (statusPhaseName st.phase) ++ ",\"title\":" ++ q st.title ++
        ",\"action\":" ++ actionName st.action ++ "}"
  let loadMatches := s.load == some s.selected
  "{\"selected\":" ++ toString s.selected ++ ",\"status\":" ++ status ++
  ",\"loadPhase\":" ++ q (phaseName s.phase) ++ ",\"load\":" ++ optNat s.load ++
  ",\"reread\":" ++ (if loadMatches then toString (s.reading.isSome || s.timer) else "null") ++
  ",\"reading\":" ++ toString s.reading.isSome ++ ",\"shown\":" ++ optNat s.shown ++ "}"

/-- Runs a trace, keeping only the enabled prefix. -/
def runTrace (acts : List Act) : List Act × List String :=
  let rec go (d : Driver) : List Act → List Act × List String
    | [] => ([], [])
    | a :: rest =>
        match apply d a with
        | none => ([], [])
        | some d' =>
            let (as, os) := go d' rest
            (a :: as, obsJson d' :: os)
  let (as, os) := go start acts
  (as, obsJson start :: os)

def traceJson (acts : List Act) : String :=
  let (as, os) := runTrace acts
  "{\"acts\":[" ++ String.intercalate "," (as.map actJson) ++ "],\"obs\":[" ++
    String.intercalate "," os ++ "]}"

/-- Every enabled sequence of up to `depth` actions from the start. -/
def exhaustive : Nat → Driver → List (List Act)
  | 0, _ => [[]]
  | depth + 1, d =>
      [] :: (enabled d).flatMap fun a =>
        match apply d a with
        | none => []
        | some d' => (exhaustive depth d').map (a :: ·)

/-- Pseudo-random walks over the enabled actions, reproducible from `seed`. -/
def walk (seed : Nat) : Nat → Driver → List Act
  | 0, _ => []
  | n + 1, d =>
      let acts := enabled d
      if acts.isEmpty then []
      else
        let next := (seed * 1103515245 + 12345) % 2147483648
        let a := acts[(next / 65536) % acts.length]!
        match apply d a with
        | none => []
        | some d' => a :: walk next n d'

def waitingStreak (r : Reply) : List Act :=
  (List.range rereadBudget).flatMap (fun _ => [.complete r, .fire]) ++
    [.complete r, .retry, .complete r, .notice, .complete .found]

/-- Scenarios a walk rarely reaches: a spent streak in each waiting phase, a
streak a notice resets, the client stopping while a read or a re-read waits,
a retry while it is stopped, a selection while it is stopped, and a mismatched reply. -/
def scripted : List (List Act) :=
  [.pending, .delivering, .notConnected, .accepted].map waitingStreak ++
  [ [.complete .pending, .fire, .complete .pending, .notice, .complete .pending, .fire,
      .complete .pending, .fire, .complete .pending, .fire, .complete .pending, .fire,
      .complete .pending, .fire, .complete .pending],
    [.stop, .retry, .complete .found, .restart, .complete .found],
    [.complete .pending, .stop, .retry, .complete .pending, .restart, .complete .pending, .fire],
    [.complete .notConnected, .fire, .stop, .select true, .restart, .complete .found,
      .select false, .complete .missing],
    [.complete .mismatch, .retry, .complete .mismatch, .notice, .complete .found],
    [.complete .found, .drop, .retry, .complete .found, .select false, .complete .pending,
      .select false, .complete .found] ]

def traceRows : List String :=
  (exhaustive 3 start).map traceJson ++
  ((List.range 200).map fun i => traceJson (walk (i + 1) 30 start)) ++
  scripted.map traceJson

def casesJson : String :=
  "{\"statuses\":[" ++ String.intercalate "," statusRows ++
  "],\"operational\":[" ++ String.intercalate "," operationalRows ++
  "],\"matches\":[" ++ String.intercalate "," matchRows ++
  "],\"rereadBudget\":" ++ toString rereadBudget ++
  ",\"rereads\":[" ++ String.intercalate "," rereadRows ++
  "],\"traces\":[" ++ String.intercalate "," traceRows ++ "]}"

end Conformance.ClientSessionLoad

def main : IO Unit := IO.println Conformance.ClientSessionLoad.casesJson
