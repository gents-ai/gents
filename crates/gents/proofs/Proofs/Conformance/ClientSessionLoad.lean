import Proofs.Clients.ClientShell.SessionLoad

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
  | .loaded => "loaded" | .failed => "failed"
  | .cached => "cached"

def layerName : Layer → String
  | .localDatabase => "localDatabase" | .sessionSync => "sessionSync"
  | .sync => "sync" | .runtime => "runtime" | .inference => "inference"

def actionName : Option Action → String
  | none => "null"
  | some .retryLocal => q "retryLocal"
  | some .reloadSession => q "reloadSession"
  | some .configureInference => q "configureInference"

def statusPhaseName : StatusPhase → String
  | .loading => "loading" | .blocked => "blocked" | .failed => "failed"

def operationalLayerName : OperationalLayer → String
  | .client => "client" | .selection => "selection" | .sync => "sync"
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
  [.idle, .loading, .pending, .loaded, .failed, .cached]

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
  [.client, .selection, .sync, .route, .runtime, .inference, .reconcile]

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
the observer's drain loop starts any owed read, one pass at a time, once its
burst window has passed (`settle`). `elapse` is the replay's clock passing the
window. Each action is a `Step` (`apply_reachable`), so every observed state is
`Reachable` and `phase_has_exit` covers it. The retry action is taken only
while the projected status offers it and no read runs. A shown session dropped
while its refresh runs offers it beside that read; production then holds two
reads whose older answer fails the refresh fence, and a trace answers only the
latest, so the driver leaves that case out. The replay's clock moves only at
`elapse`, `fire` and `restart`, so an open window opened no earlier than an
armed re-read and, being shorter than every re-read delay, ends first: `fire`
is taken only with the window closed. -/

inductive Reply where
  | found | missing | pending | failed | refused
  | mismatch
  deriving DecidableEq, Repr

def Reply.outcome : Reply → Outcome
  | .found => .found
  | .missing => .missing
  | .pending => .waiting .pending rfl
  | .failed => .failed
  | .refused => .refused
  | .mismatch => .mismatch

def Reply.name : Reply → String
  | .found => "found" | .missing => "missing" | .pending => "pending"
  | .failed => "failed" | .refused => "refused"
  | .mismatch => "mismatch"

def replies : List Reply :=
  [.found, .missing, .pending, .failed, .refused,
    .mismatch]

inductive Act where
  | select (drop : Bool)
  | notice
  | complete (r : Reply)
  | fire
  | retry
  | drop
  | stop
  | restart
  | elapse
  deriving DecidableEq, Repr, Inhabited

def settle (s : State) : State :=
  if s.running && s.queued && !s.busy && !s.window then beginRead s else s

def retryOffered (s : State) : Bool :=
  s.reading.isNone && (project (input s none)).bind (·.action) == some .retryLocal

def other (n : Nat) : Nat := if n == 0 then 1 else 0

def selectState (s : State) (drop : Bool) : State := selectTarget s (other s.selected) drop

def apply (s : State) : Act → Option State
  | .select drop => some (settle (selectState s drop))
  | .notice => if s.running then some (settle { s.owe with fresh := true }) else none
  | .complete r =>
      match s.reading with
      | some t => some (settle (finish s t r.outcome))
      | none => none
  | .fire =>
      if s.timer && !s.window then some (settle { s with timer := false, fired := true }.owe)
      else none
  | .retry =>
      if retryOffered s then some (settle { launch s with budget := rereadBudget }) else none
  | .drop => some (settle { s with shown := none })
  | .stop => if s.running then some (stopObserver s) else none
  | .restart =>
      if !s.running then some (settle { s with running := true, fresh := true }.owe) else none
  | .elapse => if s.window then some (settle { s with window := false }) else none

theorem settle_reachable (s : State) (h : Reachable s) : Reachable (settle s) := by
  unfold settle
  split
  · rename_i hc
    simp only [Bool.and_eq_true, Bool.not_eq_true'] at hc
    exact Reachable.step h (Step.begin _ hc.1.1.2 hc.1.2 hc.2)
  · exact h

theorem other_ne (n : Nat) : other n ≠ n := by
  unfold other
  split <;> rename_i h <;> simp only [beq_iff_eq] at h <;> omega

theorem apply_reachable (d d' : State) (a : Act) (hs : Reachable d)
    (h : apply d a = some d') : Reachable d' := by
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
        rename_i ht
        simp only [Bool.and_eq_true] at ht
        exact settle_reachable _ (Reachable.step hs (Step.fire _ ht.1))
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
  | elapse =>
      simp only [apply] at h; split at h
      · simp only [Option.some.injEq] at h; subst h
        rename_i hw; exact settle_reachable _ (Reachable.step hs (Step.elapse _ hw))
      · cases h

/-- The replay starts once the first read's window has passed. -/
def start : State := settle { initial 0 with window := false }

theorem start_reachable : Reachable start :=
  settle_reachable _ (Reachable.step (Reachable.init 0) (Step.elapse _ rfl))

def allActs : List Act :=
  [.select false, .select true, .drop, .notice, .stop, .restart, .fire, .retry, .elapse] ++
    replies.map .complete

def enabled (d : State) : List Act := allActs.filter fun a => (apply d a).isSome

def actJson : Act → String
  | .select drop => "{\"kind\":\"select\",\"drop\":" ++ toString drop ++ "}"
  | .notice => "{\"kind\":\"notice\"}"
  | .complete r => "{\"kind\":\"complete\",\"reply\":" ++ q r.name ++ "}"
  | .fire => "{\"kind\":\"fire\"}"
  | .retry => "{\"kind\":\"retry\"}"
  | .drop => "{\"kind\":\"drop\"}"
  | .stop => "{\"kind\":\"stop\"}"
  | .restart => "{\"kind\":\"restart\"}"
  | .elapse => "{\"kind\":\"elapse\"}"

private def optNat : Option Nat → String
  | none => "null"
  | some n => toString n

/-- What a trace compares after each action. The status detail carries error
text the model abstracts, so the projection rows above check it instead. -/
def obsJson (s : State) : String :=
  let status := match project (input s none) with
    | none => "null"
    | some st =>
        "{\"layer\":" ++ q (layerName st.layer) ++ ",\"phase\":" ++
        q (statusPhaseName st.phase) ++ ",\"title\":" ++ q st.title ++
        ",\"action\":" ++ actionName st.action ++ "}"
  let loadMatches := s.load == some s.selected
  "{\"selected\":" ++ toString s.selected ++ ",\"status\":" ++ status ++
  ",\"loadPhase\":" ++ q (phaseName s.phase) ++ ",\"load\":" ++ optNat s.load ++
  ",\"reread\":" ++ (if loadMatches then toString s.loadReread else "null") ++
  ",\"reading\":" ++ toString s.reading.isSome ++ ",\"shown\":" ++ optNat s.shown ++ "}"

/-- Runs a trace, keeping only the enabled prefix. -/
def runTrace (acts : List Act) : List Act × List String :=
  let rec go (d : State) : List Act → List Act × List String
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
def exhaustive : Nat → State → List (List Act)
  | 0, _ => [[]]
  | depth + 1, d =>
      [] :: (enabled d).flatMap fun a =>
        match apply d a with
        | none => []
        | some d' => (exhaustive depth d').map (a :: ·)

/-- Pseudo-random walks over the enabled actions, reproducible from `seed`. -/
def walk (seed : Nat) : Nat → State → List Act
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

/-- A scripted trace with the burst window passed wherever its next act waits
on it. -/
def paced (acts : List Act) : List Act :=
  let rec go (d : State) : List Act → List Act
    | [] => []
    | a :: rest =>
        match apply d a with
        | some d' => a :: go d' rest
        | none =>
            match apply d .elapse with
            | none => []
            | some e =>
                match apply e a with
                | some d' => .elapse :: a :: go d' rest
                | none => []
  go start acts

def waitingStreak (r : Reply) : List Act :=
  (List.range rereadBudget).flatMap (fun _ => [.complete r, .fire]) ++
    [.complete r, .retry, .complete r, .notice, .complete .found]

/-- Scenarios a walk rarely reaches: a spent streak in each waiting phase, a
streak a notice resets, the client stopping while a read or a re-read waits,
a retry while it is stopped, a selection while it is stopped, a mismatched reply,
a refresh of a shown session that is dropped, stopped or answered with a
waiting phase, recent reads: reopened and confirmed, refused, reopened while
another read runs, settled without confirmation, and stopped; and the drain
loop's passes: requests made inside one window read by one pass, and a store
notice made while a pass runs read by the next pass, which starts as that pass
ends. -/
def scriptedActs : List (List Act) :=
  [.pending].map waitingStreak ++
  [ [.complete .pending, .fire, .complete .pending, .notice, .complete .pending, .fire,
      .complete .pending, .fire, .complete .pending, .fire, .complete .pending, .fire,
      .complete .pending, .fire, .complete .pending],
    [.stop, .retry, .complete .found, .restart, .complete .found],
    [.complete .pending, .stop, .retry, .complete .pending, .restart, .complete .pending, .fire],
    [.complete .pending, .fire, .stop, .select true, .restart, .complete .found,
      .select false, .complete .missing],
    [.complete .mismatch, .retry, .complete .mismatch, .notice, .complete .found],
    [.complete .found, .drop, .retry, .complete .found, .select false, .complete .pending,
      .select false, .complete .found],
    [.complete .found, .notice, .drop, .complete .found, .drop, .retry, .complete .found,
      .notice, .complete .found],
    [.complete .found, .notice, .stop, .restart, .complete .found, .notice,
      .complete .pending, .fire, .complete .found],
    [.complete .found, .notice, .complete .missing, .notice, .complete .found],
    [.complete .found, .select false, .complete .found, .select false, .complete .found,
      .select true, .complete .refused, .select false, .select false, .complete .found],
    [.complete .found, .select false, .complete .found, .notice, .select false,
      .complete .found, .complete .found],
    [.complete .found, .select false, .complete .found, .select false, .complete .pending,
      .select false, .complete .failed, .select false, .complete .mismatch],
    [.complete .found, .select false, .complete .found, .select false, .stop, .restart,
      .complete .found],
    [.complete .found, .select false, .complete .found, .select false, .complete .missing,
      .select false, .select false, .complete .found],
    [.complete .found, .select false, .complete .found, .notice, .select false,
      .complete .refused, .complete .found, .select false],
    [.complete .found, .notice, .notice, .select false, .elapse, .complete .found, .notice,
      .elapse, .complete .found],
    [.notice, .complete .pending, .complete .pending, .notice, .elapse, .notice,
      .complete .pending, .complete .found] ]

def scripted : List (List Act) := scriptedActs.map paced

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
