namespace ClientObservationScope

structure State where
  requested : Option Nat
  attachments : Nat
  deriving DecidableEq, Repr

inductive Action where
  | select (node : Option Nat)
  | attach
  | detach
  deriving DecidableEq, Repr

/-- An attached frontend may observe another node than the embedding shell.
The shared observer must retain the full authorized scope until it detaches;
this changes observation focus, never document authorization. Each runtime scope
is observed through its authenticated endpoint, using the client DID. Missing or
unavailable endpoints cannot be replaced by locally replicated runtime documents. -/
def effective (state : State) : Option Nat :=
  if state.attachments = 0 then state.requested else none

def step (state : State) : Action → State
  | .select node => { state with requested := node }
  | .attach => { state with attachments := state.attachments + 1 }
  | .detach => { state with attachments := state.attachments - 1 }

theorem attached_scope_is_open (state : State) (h : 0 < state.attachments) :
    effective state = none := by
  simp [effective, Nat.ne_of_gt h]

theorem selection_cannot_narrow_an_attached_observer
    (state : State) (node : Option Nat) (h : 0 < state.attachments) :
    effective (step state (.select node)) = none := by
  simp [step, effective, Nat.ne_of_gt h]

theorem last_detachment_restores_current_intent
    (state : State) (h : state.attachments = 1) :
    effective (step state .detach) = state.requested := by
  simp [step, effective, h]

theorem attachment_and_detachment_preserve_focus (state : State) :
    effective (step (step state .attach) .detach) = effective state := by
  simp [step]

theorem selection_during_attachment_survives_detachment
    (state : State) (node : Option Nat) (h : state.attachments = 1) :
    effective (step (step state (.select node)) .detach) = node := by
  simp [step, effective, h]

theorem detached_count_stays_zero (node : Option Nat) :
    step { requested := node, attachments := 0 } .detach =
      { requested := node, attachments := 0 } := by
  rfl

end ClientObservationScope

/-! Several desktop windows, each on its own node. A window is registered by
its native creation and retired only by its native destruction; each holds its
own node selection, pack watches, prompt subscriptions and store view. The
runtime observer is shared and node-scoped (one source per runtime, TLA
`RuntimeConnectionTransport`), so a window reads its node's slice of it. TLA
`WindowIndependence` checks delivery and convergence across two windows. -/
namespace ClientWindowScope

/-- `none`: the window is not registered (never created, or destroyed). -/
abbrev Selections := Nat → Option (Option Nat)

def select (s : Selections) (window : Nat) (node : Option Nat) : Selections :=
  fun other => if other = window then (s window).map (fun _ => node) else s other

def destroy (s : Selections) (window : Nat) : Selections :=
  fun other => if other = window then none else s other

/-- The node a window's command acts on: the node it names, else the calling
window's own selection. No other window's selection is consulted, so a window
never acts on the node another window selected. -/
def commandNode (s : Selections) (window : Nat) (explicit : Option Nat) : Option Nat :=
  explicit <|> (s window).join

/-- A node-scoped delivery (a pack watch event, a node's store slice) reaches
a window only while it selects that node. -/
def delivers (s : Selections) (window node : Nat) : Bool :=
  decide ((s window).join = some node)

theorem select_preserves_other_windows (s : Selections) (window other : Nat)
    (node : Option Nat) (explicit : Option Nat) (h : other ≠ window) :
    commandNode (select s window node) other explicit = commandNode s other explicit := by
  simp [commandNode, select, h]

theorem destroy_preserves_other_windows (s : Selections) (window other : Nat)
    (explicit : Option Nat) (h : other ≠ window) :
    commandNode (destroy s window) other explicit = commandNode s other explicit := by
  simp [commandNode, destroy, h]

theorem explicit_node_wins (s : Selections) (window node : Nat) :
    commandNode s window (some node) = some node := rfl

theorem selected_window_acts_on_its_node (s : Selections) (window node : Nat)
    (h : s window = some (some node)) :
    commandNode s window none = some node := by
  simp [commandNode, h]

/-- An unregistered window has no fallback; a late command from a destroyed
window acts on nothing. -/
theorem destroyed_window_has_no_fallback (s : Selections) (window : Nat) :
    commandNode (destroy s window) window none = none := by
  simp [commandNode, destroy]

/-- Selecting another node stops deliveries of the old one to that window. -/
theorem switched_window_rejects_old_node (s : Selections) (window old new : Nat)
    (registered : (s window).isSome) (h : old ≠ new) :
    delivers (select s window (some new)) window old = false := by
  cases hs : s window with
  | none => simp [hs] at registered
  | some _ => simp [delivers, select, hs, Ne.symm h]

/-- No delivery crosses windows: one window's selection never changes what
another window is delivered. -/
theorem select_preserves_other_deliveries (s : Selections) (window other node : Nat)
    (selected : Option Nat) (h : other ≠ window) :
    delivers (select s window selected) other node = delivers s other node := by
  simp [delivers, select, h]

/-- Two windows on two nodes: each acts on and is delivered only its own. -/
theorem two_windows_stay_independent (s : Selections) (a b na nb : Nat)
    (hab : a ≠ b) (hn : na ≠ nb) (ha : (s a).isSome) (hb : (s b).isSome) :
    let t := select (select s a (some na)) b (some nb)
    commandNode t a none = some na ∧ commandNode t b none = some nb ∧
      delivers t a nb = false ∧ delivers t b na = false := by
  cases hsa : s a with
  | none => simp [hsa] at ha
  | some _ =>
    cases hsb : s b with
    | none => simp [hsb] at hb
    | some _ =>
      simp [commandNode, delivers, select, hsa, hsb, hab, Ne.symm hab, hn, Ne.symm hn]

end ClientWindowScope
