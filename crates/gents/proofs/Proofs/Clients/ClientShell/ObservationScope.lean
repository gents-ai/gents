/-! Several desktop windows, each on its own node. A window is registered by
its native creation and retired only by its native destruction; each holds its
own node selection, pack watches, prompt subscriptions and store view. Each
runtime is observed through its own connection into its own store (one source
and one store per runtime, TLA `RuntimeConnectionTransport`); no store holds
another node's rows, so a window reads its node's own store and the fleet view
summarizes each node from that node's store. A connection is the runtime's
authenticated endpoint under the client DID; a missing or unavailable endpoint
is never replaced by locally replicated runtime documents, and observation never
changes document authorization. TLA `WindowIndependence` checks
delivery and convergence across two windows. -/
namespace ClientWindowScope

/-- `none`: the window is not registered (never created, or destroyed). -/
abbrev Selections := Nat → Option (Option Nat)

/-- A window's native creation registers it with no node selected; a window
already registered keeps its selection. -/
def register (s : Selections) (window : Nat) : Selections :=
  fun other => if other = window then some (s window).join else s other

def select (s : Selections) (window : Nat) (node : Option Nat) : Selections :=
  fun other => if other = window then (s window).map (fun _ => node) else s other

def destroy (s : Selections) (window : Nat) : Selections :=
  fun other => if other = window then none else s other

/-- The node a window's command acts on: the node it names, else the calling
window's own selection. No other window's selection is consulted, so a window
never acts on the node another window selected. -/
def commandNode (s : Selections) (window : Nat) (explicit : Option Nat) : Option Nat :=
  explicit <|> (s window).join

/-- A node-scoped delivery (a pack watch event, a node's store) reaches
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

theorem register_preserves_other_windows (s : Selections) (window other : Nat)
    (explicit : Option Nat) (h : other ≠ window) :
    commandNode (register s window) other explicit = commandNode s other explicit := by
  simp [commandNode, register, h]

theorem register_keeps_a_selection (s : Selections) (window : Nat) (explicit : Option Nat) :
    commandNode (register s window) window explicit = commandNode s window explicit := by
  simp [commandNode, register]

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
