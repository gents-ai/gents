/-!
Selected-session load presentation and its follow-up obligation.

The desktop reads a selected session from its runtime over authenticated HTTP.
Store and pairing-health notices are the only pushed wakes; a session whose
row has not reached its owner yet (`projectionPending`) can have no further
notice at all, because the runtime holds no replica here. So every status the shell shows for the selected session must
either be making progress on its own (a read in flight, a read queued, or a
re-read scheduled) or name its cause and offer an action. Nothing waits on a
notice that may never come.

The projection mirrors `projectSessionLoadingStatus` in the desktop's
`lib/loadingStatus.ts`. The follow-up machine mirrors `createSessionReads`
(`hooks/sessionReads.ts`), which owns the load, its re-read streak and the
refresh fence, and the observer's drain loop (`hooks/projectionController.ts`
and `hooks/clientObservation.ts`), which starts the reads that notices,
selection changes and the re-read timer request, one at a time. Both are
replayed against production by `Conformance.ClientSessionLoad`. The
operational admission blocker is projected verbatim from the
runtime-connection owner, whose own model carries its exits.

The shell keeps the last few sessions it read in memory, so reopening one
shows that read at once in phase `cached` while it is read again. The copy is
on screen only while its read is on the way, and the read settles the screen
as it would have without the copy: a confirmed read keeps it, any other
outcome clears it, and a refused read also forgets it.
-/
namespace ClientSessionLoad

inductive Phase where
  | idle | loading | pending | loaded | failed
  /-- a recent read of the selection is shown while its read runs or is owed -/
  | cached
  deriving DecidableEq, Repr

/-- A phase that waits on the runtime rather than on a read the shell runs:
the selected request has not reached its owner. This is the phase a re-read
must follow. The desktop reads its runtime directly, so there is no
message-delivery phase between a written request and its session. -/
def Phase.isWaiting (p : Phase) : Bool := p == .pending

inductive Layer where
  | localDatabase | p2p | sessionSync | sync | runtime | inference
  deriving DecidableEq, Repr

inductive Action where
  | retryLocal | reloadSession | configureInference
  deriving DecidableEq, Repr

inductive StatusPhase where
  | loading | blocked | failed
  deriving DecidableEq, Repr

structure Status where
  layer : Layer
  phase : StatusPhase
  title : String
  detail : String
  action : Option Action
  deriving DecidableEq, Repr

inductive OperationalLayer where
  | client | selection | sync | p2p | route | runtime | inference | reconcile
  deriving DecidableEq, Repr

inductive OperationalKind where
  | ready | working | waiting | syncing | blocked
  deriving DecidableEq, Repr

/-- The runtime-connection owner's admission blocker. Its label, detail and
action are that owner's; this projection only places it. -/
structure Blocker where
  layer : OperationalLayer
  kind : OperationalKind
  label : String
  detail : String
  configureInference : Bool
  deriving DecidableEq, Repr

/-- `matches` is the shared selection match: the same session, and the same
node unless either side does not name one. -/
def matchesSelection (selectedSession target : Nat) (selectedNode targetNode : Option Nat) :
    Bool :=
  selectedSession == target &&
    (selectedNode.isNone || targetNode.isNone || selectedNode == targetNode)

structure Input where
  selected : Bool
  sessionMatches : Bool
  loadMatches : Bool
  phase : Phase
  error : Option String
  /-- `some false`: the runtime answered that the session does not exist. -/
  found : Option Bool
  /-- A read of this load is running or scheduled (`SessionLoadState.reread`). -/
  reread : Bool
  blocker : Option Blocker
  deriving DecidableEq, Repr

def operationalStatus (b : Blocker) : Status :=
  { layer :=
      match b.layer with
      | .p2p | .route => .p2p
      | .sync => .sync
      | .inference => .inference
      | _ => .runtime
    phase := if b.kind == .blocked then .blocked else .loading
    title := b.label
    detail := b.detail
    action := if b.configureInference then some .configureInference else none }

/-- With no read running or scheduled, a waiting phase offers the narrowest
recovery the shell has: read the selected session again. Its wording and
whether it should instead reopen the session are a product decision; until
then the existing title stays and only the action is added. -/
def followUpAction (reread : Bool) : Option Action :=
  if reread then none else some .retryLocal

def waitingStatus (reread : Bool) : Status :=
  { layer := .sessionSync, phase := .loading, title := "Waiting for session update"
    detail := "Waiting for the latest conversation update."
    action := followUpAction reread }

def retryingStatus (sessionMatches : Bool) : Status :=
  { layer := .localDatabase, phase := .loading, title := "Retrying conversation update"
    detail := if sessionMatches
      then "The last update failed. Displayed messages may be out of date."
      else "The last read failed. Retrying this conversation."
    action := none }

def loadingStatus : Status :=
  { layer := .localDatabase, phase := .loading, title := "Loading session"
    detail := "Reading saved messages from the runtime.", action := none }

def failedStatus (error : Option String) : Status :=
  { layer := .localDatabase, phase := .failed, title := "Couldn’t load this session"
    detail := error.getD "The runtime read failed.", action := some .retryLocal }

/-- A recent read of the selection is shown while it is read again: the shown
copy is not claimed current until that read lands. -/
def cachedStatus : Status :=
  { layer := .localDatabase, phase := .loading, title := "Updating session"
    detail := "Showing the last read of this session while it is read again."
    action := none }

def openingStatus : Status :=
  { layer := .localDatabase, phase := .loading, title := "Opening session"
    detail := "Preparing the runtime session read.", action := none }

/-- The selected session's load names it, no read is running, and yet the
session is not shown (a writer dropped the shown session without moving the
selection). No read follows on its own, so it offers one. -/
def unshownStatus : Status :=
  { layer := .localDatabase, phase := .loading, title := "Loading session"
    detail := "Reading the session from its runtime.", action := some .retryLocal }

def project (i : Input) : Option Status :=
  if !i.selected then none
  else if i.loadMatches && i.phase == .pending && !i.sessionMatches then
    some (waitingStatus i.reread)
  else if i.loadMatches && i.phase == .loading && i.error.isSome then
    some (retryingStatus i.sessionMatches)
  else if i.loadMatches && i.phase == .loading && !i.sessionMatches then
    some loadingStatus
  else if i.loadMatches && i.phase == .cached then
    some (if i.sessionMatches then cachedStatus else loadingStatus)
  else if i.loadMatches && i.phase == .failed then
    some (failedStatus i.error)
  else if !i.loadMatches && !i.sessionMatches then
    some openingStatus
  else if !i.sessionMatches then
    if i.loadMatches && i.phase == .loaded && i.found == some false then none
    else some unshownStatus
  else i.blocker.map operationalStatus

/-- What a status without an action relies on to move. -/
inductive Exit where
  /-- the selected load's read (of a `loading` or `cached` load) is running or
  owed and will settle its phase -/
  | readRunning
  /-- the load names another target; the read for the selection is queued -/
  | readQueued
  /-- a waiting phase's re-read is running or scheduled -/
  | reread
  /-- the runtime-connection owner's blocker, whose exits that model owns -/
  | operational
  deriving DecidableEq, Repr

/-- Every status the projection shows without an action falls into exactly
one of the `Exit` classes, read off its input. -/
theorem actionless_status_has_exit (i : Input) (s : Status)
    (h : project i = some s) (hn : s.action = none) :
    (i.loadMatches = true ∧ (i.phase = .loading ∨ i.phase = .cached)) ∨
      i.loadMatches = false ∨
      (i.phase.isWaiting = true ∧ i.reread = true) ∨
      (i.sessionMatches = true ∧ i.blocker.isSome) := by
  unfold project at h
  cases hs : i.selected
  · simp [hs] at h
  simp only [hs, Bool.not_true, Bool.false_eq_true, ↓reduceIte] at h
  · by_cases h1 : (i.loadMatches && i.phase == .pending && !i.sessionMatches) = true
    · rw [if_pos h1] at h
      cases h
      simp only [waitingStatus, followUpAction] at hn
      split at hn
      · simp only [Bool.and_eq_true, beq_iff_eq] at h1
        exact Or.inr (Or.inr (Or.inl ⟨by simp [Phase.isWaiting, h1.1.2], by assumption⟩))
      · cases hn
    rw [if_neg h1] at h
    by_cases h3 : (i.loadMatches && i.phase == .loading && i.error.isSome) = true
    · simp only [Bool.and_eq_true, beq_iff_eq] at h3
      exact Or.inl ⟨h3.1.1, Or.inl h3.1.2⟩
    rw [if_neg h3] at h
    by_cases h4 : (i.loadMatches && i.phase == .loading && !i.sessionMatches) = true
    · simp only [Bool.and_eq_true, beq_iff_eq] at h4
      exact Or.inl ⟨h4.1.1, Or.inl h4.1.2⟩
    rw [if_neg h4] at h
    by_cases hc : (i.loadMatches && i.phase == .cached) = true
    · simp only [Bool.and_eq_true, beq_iff_eq] at hc
      exact Or.inl ⟨hc.1, Or.inr hc.2⟩
    rw [if_neg hc] at h
    by_cases h5 : (i.loadMatches && i.phase == .failed) = true
    · rw [if_pos h5] at h
      cases h
      simp [failedStatus] at hn
    rw [if_neg h5] at h
    by_cases h6 : (!i.loadMatches && !i.sessionMatches) = true
    · simp only [Bool.and_eq_true, Bool.not_eq_true'] at h6
      exact Or.inr (Or.inl h6.1)
    rw [if_neg h6] at h
    by_cases h7 : (!i.sessionMatches) = true
    · rw [if_pos h7] at h
      split at h
      · cases h
      · cases h
        simp [unshownStatus] at hn
    rw [if_neg h7] at h
    simp only [Bool.not_eq_true', Bool.not_eq_false] at h7
    cases hb : i.blocker with
    | none => simp [hb] at h
    | some b => exact Or.inr (Or.inr (Or.inr ⟨h7, by simp⟩))

/-! ## Follow-up machine

One selected session at a time. Targets are abstract session numbers; two
targets are the same when `matchesSelection` holds, so a read whose node
override narrows the selection's node is a read of the selection, and only a
reply naming another session is a mismatch. The observer's drain loop starts
one read at a time. The shell's retry action and the other direct callers of
`refreshSession` may start one beside it, so `reading` holds only the latest
refresh: an older read's result fails the refresh fence and is dropped. -/

/-- The re-read budget a waiting phase is given. The production delays back off
and are capped; only their number is modeled. -/
def rereadBudget : Nat := 5

/-- A waiting outcome arms a re-read while budget remains. -/
def rereadArmed (budget : Nat) : Bool := decide (0 < budget)

/-- How many recently read sessions the shell keeps. They are held in memory
for the app run only, keyed by node and session; a target here stands for
both. -/
def cacheBound : Nat := 8

/-- Drops a target's recent read. -/
def forgetRecent (t : Nat) (recent : List Nat) : List Nat := recent.filter (· != t)

/-- Keeps a target's latest read, most recent first, within the bound. -/
def rememberRecent (t : Nat) (recent : List Nat) : List Nat :=
  (t :: forgetRecent t recent).take cacheBound

structure State where
  selected : Nat
  /-- the target the load state names; `none` is the idle load -/
  load : Option Nat
  phase : Phase
  shown : Option Nat
  found : Option Bool
  /-- the load carries an error: its last read failed or reported a waiting cause -/
  errored : Bool
  /-- the latest refresh's target while it runs -/
  reading : Option Nat
  /-- the drain loop owes a read of the selection -/
  queued : Bool
  /-- the owed read starts a new re-read streak: something other than the
  re-read timer asked for it -/
  fresh : Bool
  /-- a waiting phase's re-read is scheduled -/
  timer : Bool
  budget : Nat
  /-- the observer runs: the client is running, so its listener, drain loop and
  re-read timer are live -/
  running : Bool
  /-- the targets whose last read is kept, most recent first -/
  recent : List Nat
  deriving DecidableEq, Repr

inductive Outcome where
  | found
  | missing
  | waiting (phase : Phase) (h : phase.isWaiting = true)
  | failed
  /-- the runtime refused the read (`permissionDenied`): access was withdrawn -/
  | refused
  /-- the reply names another session than the read asked for -/
  | mismatch

/-- The runtime withdrew access to the target. -/
def Outcome.withdraws : Outcome → Bool
  | .refused => true
  | _ => false

/-- The load names the selection, read and shown: a read of it refreshes what
is already on screen rather than loading it. -/
def State.refreshes (s : State) : Bool :=
  s.load == some s.selected && s.phase == .loaded && s.shown == some s.selected

/-- The user selects target `n`. While the observer runs, its selection
subscription requests the read and shows a recent read of `n`, if one is kept,
in phase `cached` until that read lands. Without one, `drop` is a writer
dropping the shown session with the move. -/
def selectTarget (s : State) (n : Nat) (drop : Bool) : State :=
  if s.running && s.recent.contains n then
    { s with selected := n, queued := true, fresh := true, timer := false, shown := some n,
             load := some n, phase := .cached, found := none, errored := false }
  else
    { s with selected := n, queued := s.queued || s.running, fresh := s.fresh || s.running,
             timer := false, shown := if drop then none else s.shown }

/-- A read of the selection starts and takes over the load. A waiting phase of
the same target stays while it is read again, as does a shown recent read,
which says it is not current until the read lands; and a shown session stays
`loaded` while it is refreshed: store notices re-read the open session on
activity anywhere on its node, and each such read reporting `loading` would
reload the whole session view for a read that usually finds nothing new. Its
load is left as it was, so the read is carried by the refresh fence alone. Any
read cancels the scheduled re-read. -/
def launch (s : State) : State :=
  if s.refreshes then { s with reading := some s.selected, timer := false }
  else
    let same := s.load == some s.selected
    { s with
      reading := some s.selected
      load := some s.selected
      phase := if same && (s.phase.isWaiting || s.phase == .cached) then s.phase else .loading
      found := if same && s.found == some false then some false else none
      errored := same && s.errored
      timer := false }

/-- The drain loop starts the owed read; a read the timer alone asked for
continues its streak. -/
def beginRead (s : State) : State :=
  { launch s with queued := false, fresh := false,
                  budget := if s.fresh then rereadBudget else s.budget }

/-- What stays on screen when a read settles without confirming it: a shown
recent read is cleared, so the screen is what it would be without the copy. -/
def State.unconfirmed (s : State) : Option Nat :=
  if s.phase == .cached then none else s.shown

def finish (s : State) (target : Nat) (o : Outcome) : State :=
  if target ≠ s.selected then
    /- The selection moved while the read ran: the answer is dropped, and the
       selection subscription has already queued the moved selection's read
       (`moved_read_leaves_read_queued`). A refusal still withdraws the
       target's recent read. -/
    { s with reading := none,
             recent := if o.withdraws then forgetRecent target s.recent else s.recent }
  else
    let s := { s with reading := none, load := some target }
    match o with
    | .found =>
        { s with shown := some target, phase := .loaded, found := some true,
                 errored := false, budget := rereadBudget,
                 recent := rememberRecent target s.recent }
    | .missing =>
        { s with shown := none, phase := .loaded, found := some false, errored := false,
                 budget := rereadBudget, recent := forgetRecent target s.recent }
    | .waiting p _ =>
        { s with shown := s.unconfirmed, phase := p, found := none, errored := true,
                 timer := s.running && rereadArmed s.budget, budget := s.budget - 1 }
    | .failed =>
        { s with shown := s.unconfirmed, phase := .failed, found := none, errored := true }
    /- Access was withdrawn: nothing of the session stays on screen or in
       memory. -/
    | .refused =>
        { s with shown := none, phase := .failed, found := none, errored := true,
                 recent := forgetRecent target s.recent }
    /- A reply for another session cannot be shown, and reading again would ask
       the same runtime the same question. It fails visibly instead of leaving
       the load in `loading` with no read behind it. -/
    | .mismatch =>
        { s with shown := s.unconfirmed, phase := .failed, found := none, errored := true }

/-- The client stops: the observer drops its reads and timer. Nothing will
settle the held load, so a `loading` or `cached` load fails visibly, a shown
recent read is cleared, and a waiting phase loses its re-read, which offers
the retry action. -/
def stopObserver (s : State) : State :=
  { s with running := false, reading := none, queued := false, fresh := false,
           timer := false, shown := s.unconfirmed,
           phase := if s.phase == .loading || s.phase == .cached then .failed else s.phase,
           errored := s.errored || s.phase == .loading || s.phase == .cached }

inductive Step : State → State → Prop where
  /-- the user selects another session; while the observer runs, its selection
  subscription requests the read -/
  | select (s : State) (n : Nat) (drop : Bool) (h : n ≠ s.selected) :
      Step s (selectTarget s n drop)
  /-- a store or health notice, the listener's registration, or a foreground
  refresh. A store notice while the selection is shown is paced: any write on
  the node sends one, so its read waits out the gap since the last read
  (longer once reads keep finding nothing new) and, while the window is
  hidden, waits for the window to show, which reads in full. The read stays
  owed and is never dropped; time is not modeled, so it is owed at once. -/
  | notice (s : State) (h : s.running = true) : Step s { s with queued := true, fresh := true }
  | begin (s : State) (h : s.queued = true) : Step s (beginRead s)
  | complete (s : State) (t : Nat) (o : Outcome) (h : s.reading = some t) :
      Step s (finish s t o)
  | fire (s : State) (h : s.timer = true) : Step s { s with timer := false, queued := true }
  /-- the shell's retry action reads directly, with a fresh streak -/
  | retry (s : State) : Step s { launch s with budget := rereadBudget }
  /-- a writer drops the shown session without moving the selection -/
  | drop (s : State) : Step s { s with shown := none }
  | stop (s : State) (h : s.running = true) : Step s (stopObserver s)
  /-- the client runs again; the observer reads the selection at once -/
  | restart (s : State) (h : s.running = false) :
      Step s { s with running := true, queued := true, fresh := true }
  /-- the client's identity changes or a node is removed: the recent reads it
  covered are dropped -/
  | forget (s : State) (t : Nat) : Step s { s with recent := forgetRecent t s.recent }

/-- The observer owes the selection a read from the start. Its listener's
registration requests it; until a registration succeeds, a rejection or a
deadline (`UNREGISTERED_READ_MS`) requests it without the listener, so the first
read never waits on a registration that may not resolve. The TLA model
`DesktopUpdateListener` (`StartupRead`, `NoRegistration`) checks that race. -/
def initial (selected : Nat) : State :=
  { selected, load := none, phase := .idle, shown := none, found := none,
    errored := false, reading := none, queued := true, fresh := true, timer := false,
    budget := rereadBudget, running := true, recent := [] }

inductive Reachable : State → Prop where
  | init (n : Nat) : Reachable (initial n)
  | step {s t : State} : Reachable s → Step s t → Reachable t

/-- What the load's `reread` says: a read of the load is running or a re-read is
scheduled. A refresh of a shown session leaves its `loaded` load untouched, so
a read running under it is not on the load. -/
def State.loadReread (s : State) : Bool :=
  (s.reading.isSome && s.phase != .loaded) || s.timer

def input (s : State) (blocker : Option Blocker) : Input :=
  { selected := true
    sessionMatches := s.shown == some s.selected
    loadMatches := s.load == some s.selected
    phase := s.phase
    error := if s.errored then some "read failed" else none
    found := s.found
    reread := s.loadReread
    blocker }

/-- The machine's invariant: a named load has left `idle`. While the observer
runs, a `loading` or `cached` load is being read or owed a read, a load naming
another target is owed a read for the selection, and a read whose selection
moved leaves the moved selection's read owed. The recent reads stay within
their bound. -/
def Invariant (s : State) : Prop :=
  (s.load.isSome = true → s.phase ≠ .idle) ∧
  (s.running = true → (s.phase = .loading ∨ s.phase = .cached) →
      s.reading.isSome = true ∨ s.queued = true) ∧
  (s.running = true → s.load ≠ some s.selected → s.queued = true ∨ s.reading.isSome = true) ∧
  (s.running = true → ∀ t, s.reading = some t → t ≠ s.selected → s.queued = true) ∧
  s.recent.length ≤ cacheBound

theorem forgetRecent_length (t : Nat) (l : List Nat) : (forgetRecent t l).length ≤ l.length :=
  List.length_filter_le _ _

theorem rememberRecent_length (t : Nat) (l : List Nat) :
    (rememberRecent t l).length ≤ cacheBound := by
  unfold rememberRecent
  rw [List.length_take]
  exact Nat.min_le_left _ _

theorem not_mem_forgetRecent (t : Nat) (l : List Nat) : t ∉ forgetRecent t l := by
  simp [forgetRecent]

theorem launch_reading (s : State) : (launch s).reading = some s.selected := by
  unfold launch; split <;> rfl

theorem launch_selected (s : State) : (launch s).selected = s.selected := by
  unfold launch; split <;> rfl

theorem launch_recent (s : State) : (launch s).recent = s.recent := by
  unfold launch; split <;> rfl

theorem launch_load (s : State) : (launch s).load = some s.selected := by
  unfold launch
  split
  · rename_i h
    simp only [State.refreshes, Bool.and_eq_true, beq_iff_eq] at h
    exact h.1.1
  · rfl

theorem launch_phase_ne_idle (s : State) : (launch s).phase ≠ .idle := by
  unfold launch
  split
  · rename_i h
    simp only [State.refreshes, Bool.and_eq_true, beq_iff_eq] at h
    simp [h.1.2]
  · simp only
    split
    · rename_i h
      intro hp
      simp only [Bool.and_eq_true, Bool.or_eq_true, beq_iff_eq] at h
      rcases h.2 with h2 | h2 <;> simp [hp, Phase.isWaiting] at h2
    · simp

theorem invariant_initial (n : Nat) : Invariant (initial n) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩ <;> simp [initial]

theorem invariant_step {s t : State} (hs : Invariant s) (step : Step s t) : Invariant t := by
  obtain ⟨hi, hl, ho, hm, hb⟩ := hs
  cases step with
  | select n drop h =>
      unfold selectTarget
      split
      · exact ⟨fun _ => by simp, fun _ _ => Or.inr rfl, fun _ _ => Or.inl rfl,
          fun _ _ _ _ => rfl, hb⟩
      · refine ⟨hi, ?_, ?_, ?_, hb⟩ <;> intro hrun <;> simp only at hrun ⊢ <;> simp [hrun]
  | notice h =>
      refine ⟨hi, ?_, ?_, ?_, hb⟩ <;> intros <;> simp
  | begin h =>
      refine ⟨?_, ?_, ?_, ?_, ?_⟩
      · intro _; exact launch_phase_ne_idle s
      · intros; simp [beginRead, launch_reading]
      · intros; simp [beginRead, launch_reading]
      · intro _ t ht hne
        simp only [beginRead, launch_reading, launch_selected, Option.some.injEq] at ht hne
        exact absurd ht.symm hne
      · simp only [beginRead, launch_recent]; exact hb
  | complete target o h =>
      unfold finish
      by_cases hsel : target ≠ s.selected
      · simp only [hsel, ↓reduceIte, ne_eq, not_false_eq_true]
        refine ⟨fun hs => hi hs, ?_, ?_, fun _ _ h => by simp at h, ?_⟩
        · intro hrun _; exact Or.inr (hm hrun target h hsel)
        · intro hrun _; exact Or.inl (hm hrun target h hsel)
        · split
          · exact Nat.le_trans (forgetRecent_length _ _) hb
          · exact hb
      · have heq : target = s.selected := by simpa using hsel
        simp only [hsel, ↓reduceIte]
        subst heq
        cases o with
        | found =>
            exact ⟨fun _ => by simp, fun _ h => by simp at h, fun _ hne => absurd rfl hne,
              fun _ _ h => by simp at h, rememberRecent_length _ _⟩
        | missing =>
            exact ⟨fun _ => by simp, fun _ h => by simp at h, fun _ hne => absurd rfl hne,
              fun _ _ h => by simp at h, Nat.le_trans (forgetRecent_length _ _) hb⟩
        | waiting p hp =>
            refine ⟨fun _ hpi => ?_, fun _ hpl => ?_, fun _ hne => absurd rfl hne,
              fun _ _ h => by simp at h, hb⟩
            · simp only at hpi; subst hpi; simp [Phase.isWaiting] at hp
            · simp only at hpl
              rcases hpl with hpl | hpl <;> subst hpl <;>
                simp [Phase.isWaiting] at hp
        | failed =>
            exact ⟨fun _ => by simp, fun _ h => by simp at h, fun _ hne => absurd rfl hne,
              fun _ _ h => by simp at h, hb⟩
        | refused =>
            exact ⟨fun _ => by simp, fun _ h => by simp at h, fun _ hne => absurd rfl hne,
              fun _ _ h => by simp at h, Nat.le_trans (forgetRecent_length _ _) hb⟩
        | mismatch =>
            exact ⟨fun _ => by simp, fun _ h => by simp at h, fun _ hne => absurd rfl hne,
              fun _ _ h => by simp at h, hb⟩
  | fire h =>
      refine ⟨hi, ?_, ?_, ?_, hb⟩ <;> intros <;> simp
  | retry =>
      refine ⟨?_, ?_, ?_, ?_, ?_⟩
      · intro _; exact launch_phase_ne_idle s
      · intros; simp [launch_reading]
      · intros; simp [launch_reading]
      · intro _ t ht hne
        simp only [launch_reading, launch_selected, Option.some.injEq] at ht hne
        exact absurd ht.symm hne
      · simp only [launch_recent]; exact hb
  | drop =>
      exact ⟨hi, hl, ho, hm, hb⟩
  | stop h =>
      refine ⟨fun hs => ?_, fun h => by simp [stopObserver] at h,
        fun h => by simp [stopObserver] at h, fun h => by simp [stopObserver] at h, hb⟩
      have := hi hs
      simp only [stopObserver] at this ⊢
      split <;> simp_all
  | restart h =>
      refine ⟨hi, ?_, ?_, ?_, hb⟩ <;> intros <;> simp
  | forget t =>
      exact ⟨hi, hl, ho, hm, Nat.le_trans (forgetRecent_length _ _) hb⟩

theorem reachable_invariant {s : State} (h : Reachable s) : Invariant s := by
  induction h with
  | init n => exact invariant_initial n
  | step _ st ih => exact invariant_step ih st

/-- A read whose selection moved while it ran leaves the moved selection's read
owed: the selection subscription queued it, so the drain loop needs no second
request of its own. -/
theorem moved_read_leaves_read_queued {s : State} (hs : Reachable s) (hrun : s.running = true)
    (t : Nat) (o : Outcome) (h : s.reading = some t) (hne : t ≠ s.selected) :
    (finish s t o).queued = true := by
  have := (reachable_invariant hs).2.2.2.1 hrun t h hne
  simp [finish, hne, this]

/-- While the observer runs, nothing the selected session shows is silently
stuck: every status without an action has a read running, a read queued, a
re-read scheduled, or is the runtime-connection owner's blocker. -/
theorem phase_has_exit {s : State} (hs : Reachable s) (hrun : s.running = true)
    (blocker : Option Blocker) (st : Status) (h : project (input s blocker) = some st)
    (hn : st.action = none) :
    s.reading.isSome ∨ s.queued = true ∨ s.timer = true ∨
      (s.shown = some s.selected ∧ blocker.isSome) := by
  obtain ⟨_, hl, ho, _, _⟩ := reachable_invariant hs
  rcases actionless_status_has_exit _ _ h hn with ⟨_, hp⟩ | hm | ⟨_, hr⟩ | ⟨hm, hb⟩
  · rcases hl hrun hp with h | h
    · exact Or.inl h
    · exact Or.inr (Or.inl h)
  · have hne : s.load ≠ some s.selected := by
      intro heq; simp [input, heq] at hm
    rcases ho hrun hne with h | h
    · exact Or.inr (Or.inl h)
    · exact Or.inl h
  · simp only [input, State.loadReread, Bool.or_eq_true, Bool.and_eq_true] at hr
    rcases hr with h | h
    · exact Or.inl h.1
    · exact Or.inr (Or.inr (Or.inl h))
  · simp only [input, beq_iff_eq] at hm
    exact Or.inr (Or.inr (Or.inr ⟨hm, hb⟩))

/-- When the client stops, the load it held names its next action: no read or
timer remains behind it, so the status is the retry action or the
runtime-connection owner's blocker over a shown session. A selection made
while the client is stopped has no load of its own until the client runs
again; that status belongs to the client's lifecycle, outside this machine. -/
theorem stop_leaves_action {s : State} (hs : Reachable s)
    (hload : s.load = some s.selected) (blocker : Option Blocker) (st : Status)
    (h : project (input (stopObserver s) blocker) = some st) (hn : st.action = none) :
    (stopObserver s).shown = some s.selected ∧ blocker.isSome := by
  obtain ⟨hi, _, _, _, _⟩ := reachable_invariant hs
  have hidle := hi (by simp [hload])
  rcases actionless_status_has_exit _ _ h hn with ⟨_, hp⟩ | hm | ⟨_, hr⟩ | ⟨hm, hb⟩
  · simp only [input, stopObserver] at hp
    split at hp <;> simp_all
  · simp [input, stopObserver, hload] at hm
  · simp [input, stopObserver, State.loadReread] at hr
  · simp only [input, stopObserver, beq_iff_eq] at hm
    exact ⟨hm, hb⟩

/-- A waiting phase of the selected load with nothing running or scheduled,
and no session shown, names its next action. -/
theorem waiting_without_reread_offers_retry (i : Input) (hsel : i.selected = true)
    (hl : i.loadMatches = true) (hs : i.sessionMatches = false)
    (hw : i.phase.isWaiting = true) (hr : i.reread = false) :
    (project i).bind (·.action) = some .retryLocal := by
  obtain ⟨selected, sessionMatches, loadMatches, phase, error, found, reread, blocker⟩ := i
  simp only at hsel hl hs hw hr
  subst hsel hl hs hr
  cases phase <;>
    simp_all [project, Phase.isWaiting, waitingStatus, followUpAction]

/-- The re-read budget is finite: a session that keeps answering `pending`
stops being re-read and offers its action. -/
theorem waiting_rereads_are_bounded (budget : Nat) :
    rereadArmed budget = false ↔ budget = 0 := by
  simp [rereadArmed]

/-- A reply for another session fails visibly rather than staying `loading`. -/
theorem mismatched_reply_fails (s : State) :
    (finish s s.selected .mismatch).phase = .failed ∧
      (finish s s.selected .mismatch).reading = none := by
  simp [finish]

/-! ## Recent reads -/

/-- A shown recent read always has a way out: while the observer runs, its
read is running or owed, and stopping the observer fails it visibly. -/
theorem cached_has_exit {s : State} (hs : Reachable s) (hrun : s.running = true)
    (hc : s.phase = .cached) : s.reading.isSome = true ∨ s.queued = true :=
  (reachable_invariant hs).2.1 hrun (Or.inr hc)

/-- Stopping the observer leaves no shown recent read and no `cached` load. -/
theorem stop_clears_cached (s : State) (hc : s.phase = .cached) :
    (stopObserver s).phase = .failed ∧ (stopObserver s).shown = none := by
  simp [stopObserver, State.unconfirmed, hc]

/-- A read of the selection settles its load as `loaded`, `failed` (a refusal
included) or a waiting phase, so a `cached` load never outlives its read. -/
theorem read_resolves (s : State) (o : Outcome) :
    (finish s s.selected o).phase = .loaded ∨ (finish s s.selected o).phase = .failed ∨
      (finish s s.selected o).phase.isWaiting = true := by
  cases o with
  | waiting p hp => simp [finish]; exact Or.inr (Or.inr hp)
  | _ => simp [finish]

/-- A shown recent read stays on screen only when its read confirms the
session: any other outcome leaves the screen as it would be without the copy. -/
theorem cached_copy_needs_confirmation (s : State) (o : Outcome) (hc : s.phase = .cached)
    (hshown : (finish s s.selected o).shown = some s.selected) : o = .found := by
  cases o <;> simp [finish, State.unconfirmed, hc] at hshown ⊢

/-- A refused read withdraws the target's recent read, even when the selection
has moved on, and clears the session from the screen when it is selected. -/
theorem refused_read_clears (s : State) (t : Nat) :
    t ∉ (finish s t .refused).recent ∧
      (t = s.selected → (finish s t .refused).shown = none) := by
  constructor
  · unfold finish
    split
    · simpa [Outcome.withdraws] using not_mem_forgetRecent t s.recent
    · simpa using not_mem_forgetRecent t s.recent
  · intro h; subst h; simp [finish]

/-- A read that lands after the selection moved leaves the screen and the load
as they are: it never writes into the session shown now. -/
theorem moved_read_keeps_screen (s : State) (t : Nat) (o : Outcome) (h : t ≠ s.selected) :
    (finish s t o).shown = s.shown ∧ (finish s t o).load = s.load ∧
      (finish s t o).phase = s.phase := by
  simp [finish, h]

/-- Selecting a target shows no recent read but its own: what is shown after
the move is the new selection's recent read or what was already shown. -/
theorem select_shows_only_its_own (s : State) (n : Nat) (drop : Bool) (m : Nat)
    (h : (selectTarget s n drop).shown = some m) : m = n ∨ s.shown = some m := by
  unfold selectTarget at h
  split at h
  · simp only [Option.some.injEq] at h; exact Or.inl h.symm
  · simp only at h
    split at h
    · cases h
    · exact Or.inr h

/-- The recent reads never exceed their bound. -/
theorem recent_bounded {s : State} (hs : Reachable s) : s.recent.length ≤ cacheBound :=
  (reachable_invariant hs).2.2.2.2

end ClientSessionLoad
