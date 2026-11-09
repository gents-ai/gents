/-!
Selected-session load presentation and its follow-up obligation.

The desktop reads a selected session from its runtime over authenticated HTTP.
Store and pairing-health notices are the only pushed wakes; a session whose
row has not reached its owner yet (`projectionPending`) or whose message is
still in delivery can have no further notice at all, because the runtime holds
no replica here. So every status the shell shows for the selected session must
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
-/
namespace ClientSessionLoad

inductive Phase where
  | idle | loading | pending | delivering | notConnected | accepted | loaded | failed
  deriving DecidableEq, Repr

/-- The three message-delivery phases a session the node has not returned yet
reports (`sessionDelivering`, `sessionNotConnected`, `sessionAccepted`). -/
def Phase.isDelivery : Phase → Bool
  | .delivering | .notConnected | .accepted => true
  | _ => false

/-- A phase that waits on the runtime rather than on a read the shell runs:
the selected request has not reached its owner, or its message is still in
delivery. These are the phases a re-read must follow. -/
def Phase.isWaiting (p : Phase) : Bool := p == .pending || p.isDelivery

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

def deliveryStatus (phase : Phase) (error : Option String) (reread : Bool) : Status :=
  match phase with
  | .notConnected =>
      { layer := .p2p, phase := .blocked, title := "Not connected to the node"
        detail := error.getD
          "The message is saved on this computer and sends when the connection returns."
        action := followUpAction reread }
  | .accepted =>
      { layer := .sessionSync, phase := .loading, title := "Message picked up"
        detail := error.getD "The node accepted the message. Its session is on the way back."
        action := followUpAction reread }
  | _ =>
      { layer := .sessionSync, phase := .loading, title := "Sending message"
        detail := error.getD "Waiting for the node to pick up the message."
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
  else if i.loadMatches && i.phase.isDelivery && !i.sessionMatches then
    some (deliveryStatus i.phase i.error i.reread)
  else if i.loadMatches && i.phase == .loading && i.error.isSome then
    some (retryingStatus i.sessionMatches)
  else if i.loadMatches && i.phase == .loading && !i.sessionMatches then
    some loadingStatus
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
  /-- the selected load's read is running and will settle its phase -/
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
    (i.loadMatches = true ∧ i.phase = .loading) ∨
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
    by_cases h2 : (i.loadMatches && i.phase.isDelivery && !i.sessionMatches) = true
    · rw [if_pos h2] at h
      cases h
      have hr : i.reread = true := by
        cases hrr : i.reread
        · exfalso
          revert hn
          unfold deliveryStatus
          split <;> simp [followUpAction, hrr]
        · rfl
      simp only [Bool.and_eq_true] at h2
      exact Or.inr (Or.inr (Or.inl ⟨by simp [Phase.isWaiting, h2.1.2], hr⟩))
    rw [if_neg h2] at h
    by_cases h3 : (i.loadMatches && i.phase == .loading && i.error.isSome) = true
    · simp only [Bool.and_eq_true, beq_iff_eq] at h3
      exact Or.inl ⟨h3.1.1, h3.1.2⟩
    rw [if_neg h3] at h
    by_cases h4 : (i.loadMatches && i.phase == .loading && !i.sessionMatches) = true
    · simp only [Bool.and_eq_true, beq_iff_eq] at h4
      exact Or.inl ⟨h4.1.1, h4.1.2⟩
    rw [if_neg h4] at h
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
  deriving DecidableEq, Repr

inductive Outcome where
  | found
  | missing
  | waiting (phase : Phase) (h : phase.isWaiting = true)
  | failed
  /-- the reply names another session than the read asked for -/
  | mismatch

/-- A read of the selection starts and takes over the load. A waiting phase of
the same target stays while it is read again; any read cancels the scheduled
re-read. -/
def launch (s : State) : State :=
  let same := s.load == some s.selected
  { s with
    reading := some s.selected
    load := some s.selected
    phase := if same && s.phase.isWaiting then s.phase else .loading
    found := if same && s.found == some false then some false else none
    errored := same && s.errored
    timer := false }

/-- The drain loop starts the owed read; a read the timer alone asked for
continues its streak. -/
def beginRead (s : State) : State :=
  { launch s with queued := false, fresh := false,
                  budget := if s.fresh then rereadBudget else s.budget }

def finish (s : State) (target : Nat) (o : Outcome) : State :=
  if target ≠ s.selected then
    /- The selection moved while the read ran: the answer is dropped, and the
       selection subscription has already queued the moved selection's read
       (`moved_read_leaves_read_queued`). -/
    { s with reading := none }
  else
    let s := { s with reading := none }
    match o with
    | .found =>
        { s with shown := some target, phase := .loaded, found := some true,
                 errored := false, budget := rereadBudget }
    | .missing =>
        { s with shown := none, phase := .loaded, found := some false, errored := false,
                 budget := rereadBudget }
    | .waiting p _ =>
        { s with phase := p, found := none, errored := true,
                 timer := s.running && rereadArmed s.budget, budget := s.budget - 1 }
    | .failed => { s with phase := .failed, found := none, errored := true }
    /- A reply for another session cannot be shown, and reading again would ask
       the same runtime the same question. It fails visibly instead of leaving
       the load in `loading` with no read behind it. -/
    | .mismatch => { s with phase := .failed, found := none, errored := true }

/-- The client stops: the observer drops its reads and timer. Nothing will
settle the held load, so a `loading` load fails visibly and a waiting phase
loses its re-read, which offers the retry action. -/
def stopObserver (s : State) : State :=
  { s with running := false, reading := none, queued := false, fresh := false,
           timer := false,
           phase := if s.phase == .loading then .failed else s.phase,
           errored := s.errored || s.phase == .loading }

inductive Step : State → State → Prop where
  /-- the user selects another session; while the observer runs, its selection
  subscription requests the read -/
  | select (s : State) (n : Nat) (drop : Bool) (h : n ≠ s.selected) :
      Step s { s with selected := n, queued := s.queued || s.running,
                      fresh := s.fresh || s.running, timer := false,
                      shown := if drop then none else s.shown }
  /-- a store or health notice, the listener's registration, or a foreground
  refresh -/
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

/-- The observer owes the selection a read from the start. Its listener's
registration requests it; until a registration succeeds, a rejection or a
deadline (`UNREGISTERED_READ_MS`) requests it without the listener, so the first
read never waits on a registration that may not resolve. The TLA model
`DesktopUpdateListener` (`StartupRead`, `NoRegistration`) checks that race. -/
def initial (selected : Nat) : State :=
  { selected, load := none, phase := .idle, shown := none, found := none,
    errored := false, reading := none, queued := true, fresh := true, timer := false,
    budget := rereadBudget, running := true }

inductive Reachable : State → Prop where
  | init (n : Nat) : Reachable (initial n)
  | step {s t : State} : Reachable s → Step s t → Reachable t

def input (s : State) (blocker : Option Blocker) : Input :=
  { selected := true
    sessionMatches := s.shown == some s.selected
    loadMatches := s.load == some s.selected
    phase := s.phase
    error := if s.errored then some "read failed" else none
    found := s.found
    reread := s.reading.isSome || s.timer
    blocker }

/-- The machine's invariant: a running read is the load's, and a named load has
left `idle`. While the observer runs, a `loading` load is being read or owed a
read, a load naming another target is owed a read for the selection, and a
read whose selection moved leaves the moved selection's read owed. -/
def Invariant (s : State) : Prop :=
  (∀ t, s.reading = some t → s.load = some t) ∧
  (s.load.isSome = true → s.phase ≠ .idle) ∧
  (s.running = true → s.phase = .loading → s.reading.isSome = true ∨ s.queued = true) ∧
  (s.running = true → s.load ≠ some s.selected → s.queued = true ∨ s.reading.isSome = true) ∧
  (s.running = true → ∀ t, s.reading = some t → t ≠ s.selected → s.queued = true)

theorem launch_phase_ne_idle (s : State) : (launch s).phase ≠ .idle := by
  simp only [launch]
  split
  · rename_i h
    intro hp
    simp only [Bool.and_eq_true] at h
    simp [hp, Phase.isWaiting, Phase.isDelivery] at h
  · simp

theorem invariant_initial (n : Nat) : Invariant (initial n) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩ <;> simp [initial]

theorem invariant_step {s t : State} (hs : Invariant s) (step : Step s t) : Invariant t := by
  obtain ⟨hr, hi, hl, ho, hm⟩ := hs
  cases step with
  | select n drop h =>
      refine ⟨hr, hi, ?_, ?_, ?_⟩ <;> intro hrun <;> simp only at hrun ⊢ <;> simp [hrun]
  | notice h =>
      refine ⟨hr, hi, ?_, ?_, ?_⟩ <;> intros <;> simp
  | begin h =>
      refine ⟨?_, ?_, ?_, ?_, ?_⟩
      · intro t ht; simp [beginRead, launch] at ht ⊢; exact ht
      · intro _; exact launch_phase_ne_idle s
      · intros; simp [beginRead, launch]
      · intros; simp [beginRead, launch]
      · intro _ t ht hne; simp [beginRead, launch] at ht; exact absurd ht.symm hne
  | complete target o h =>
      have hload := hr target h
      unfold finish
      by_cases hsel : target ≠ s.selected
      · simp only [hsel, ↓reduceIte, ne_eq, not_false_eq_true]
        refine ⟨fun _ h => by simp at h, fun hs => hi hs, ?_, ?_, fun _ _ h => by simp at h⟩
        · intro hrun _; exact Or.inr (hm hrun target h hsel)
        · intro hrun _; exact Or.inl (hm hrun target h hsel)
      · have heq : target = s.selected := by simpa using hsel
        simp only [hsel, ↓reduceIte]
        subst heq
        cases o with
        | found =>
            exact ⟨fun _ h => by simp at h, fun _ => by simp, fun _ h => by simp at h,
              fun _ hne => absurd hload hne, fun _ _ h => by simp at h⟩
        | missing =>
            exact ⟨fun _ h => by simp at h, fun _ => by simp, fun _ h => by simp at h,
              fun _ hne => absurd hload hne, fun _ _ h => by simp at h⟩
        | waiting p hp =>
            refine ⟨fun _ h => by simp at h, fun _ hpi => ?_, fun _ hpl => ?_,
              fun _ hne => absurd hload hne, fun _ _ h => by simp at h⟩
            · simp only at hpi; subst hpi; simp [Phase.isWaiting, Phase.isDelivery] at hp
            · simp only at hpl; subst hpl; simp [Phase.isWaiting, Phase.isDelivery] at hp
        | failed =>
            exact ⟨fun _ h => by simp at h, fun _ => by simp, fun _ h => by simp at h,
              fun _ hne => absurd hload hne, fun _ _ h => by simp at h⟩
        | mismatch =>
            exact ⟨fun _ h => by simp at h, fun _ => by simp, fun _ h => by simp at h,
              fun _ hne => absurd hload hne, fun _ _ h => by simp at h⟩
  | fire h =>
      refine ⟨hr, hi, ?_, ?_, ?_⟩ <;> intros <;> simp
  | retry =>
      refine ⟨?_, ?_, ?_, ?_, ?_⟩
      · intro t ht; simp [launch] at ht ⊢; exact ht
      · intro _; exact launch_phase_ne_idle s
      · intros; simp [launch]
      · intros; simp [launch]
      · intro _ t ht hne; simp [launch] at ht; exact absurd ht.symm hne
  | drop =>
      exact ⟨hr, hi, hl, ho, hm⟩
  | stop h =>
      refine ⟨fun _ h => by simp [stopObserver] at h, fun hs => ?_, fun h => by
        simp [stopObserver] at h, fun h => by simp [stopObserver] at h,
        fun h => by simp [stopObserver] at h⟩
      have := hi hs
      simp only [stopObserver] at this ⊢
      split <;> simp_all
  | restart h =>
      refine ⟨hr, hi, ?_, ?_, ?_⟩ <;> intros <;> simp

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
  have := (reachable_invariant hs).2.2.2.2 hrun t h hne
  simp [finish, hne, this]

/-- While the observer runs, nothing the selected session shows is silently
stuck: every status without an action has a read running, a read queued, a
re-read scheduled, or is the runtime-connection owner's blocker. -/
theorem phase_has_exit {s : State} (hs : Reachable s) (hrun : s.running = true)
    (blocker : Option Blocker) (st : Status) (h : project (input s blocker) = some st)
    (hn : st.action = none) :
    s.reading.isSome ∨ s.queued = true ∨ s.timer = true ∨
      (s.shown = some s.selected ∧ blocker.isSome) := by
  obtain ⟨_, _, hl, ho, _⟩ := reachable_invariant hs
  rcases actionless_status_has_exit _ _ h hn with ⟨_, hp⟩ | hm | ⟨_, hr⟩ | ⟨hm, hb⟩
  · rcases hl hrun hp with h | h
    · exact Or.inl h
    · exact Or.inr (Or.inl h)
  · have hne : s.load ≠ some s.selected := by
      intro heq; simp [input, heq] at hm
    rcases ho hrun hne with h | h
    · exact Or.inr (Or.inl h)
    · exact Or.inl h
  · simp only [input, Bool.or_eq_true] at hr
    rcases hr with h | h
    · exact Or.inl h
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
    s.shown = some s.selected ∧ blocker.isSome := by
  obtain ⟨_, hi, _, _, _⟩ := reachable_invariant hs
  have hidle := hi (by simp [hload])
  rcases actionless_status_has_exit _ _ h hn with ⟨_, hp⟩ | hm | ⟨_, hr⟩ | ⟨hm, hb⟩
  · simp only [input, stopObserver] at hp
    split at hp <;> simp_all
  · simp [input, stopObserver, hload] at hm
  · simp [input, stopObserver] at hr
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
    simp_all [project, Phase.isWaiting, Phase.isDelivery, waitingStatus, deliveryStatus,
      followUpAction]

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

end ClientSessionLoad
