/-! The HTTP port a desktop-managed node home listens on. The native service
definition is always `gents server --home <HOME>`, so the home's
`network.json` is the only place a port other than the default can live. -/

namespace NodePort

/-- Who answers on the port the home would listen on, observed before its
service is installed. -/
inductive Holder where
  /-- Nothing accepts a connection. -/
  | nobody
  /-- This home's own runtime: ready, booting, outdated, or ready with a
  private status the User cannot read. -/
  | thisHome
  /-- A Gents runtime of another home, usually one started by hand. -/
  | anotherNode
  /-- A listener that does not answer as Gents. -/
  | anotherApp
  deriving DecidableEq, Repr

/-- Where the port in the home's `network.json` came from. -/
inductive PortSource where
  /-- No port is recorded; the home uses the default. -/
  | unset
  /-- Gents picked it because the port before it was taken. -/
  | picked
  /-- A person set it. -/
  | chosen
  deriving DecidableEq, Repr

inductive Settlement where
  /-- Listen where the home already would. -/
  | keep
  /-- Record a free port in `network.json` as picked and listen there. -/
  | move
  /-- Refuse, naming the other node and how to stop or move it. -/
  | blockedByNode
  /-- Refuse, naming the port the other app holds. -/
  | blockedByApp
  deriving DecidableEq, Repr

/-- Where a home's listener settles before its service is installed. `served`
is a home a runtime has served at least once: it has the served marker the
server writes with `runtime.json`, which `gents reset` leaves in place, or
`runtime.json` itself.

Only a home that has never served moves, and only off a port nobody chose.
Nothing has saved its address yet, and clients learn the port it serves on
from the `runtime.json` the server writes once it listens. A port Gents
picked is picked again while the home has never served, so a pick that was
taken before the first start does not strand the home. A home that has
served, or whose port a person chose, keeps it: saved connections and the
person's own configuration name that port, so the holder is reported
instead. This home's own runtime is never moved away from or launched over.

External premise: a candidate is free only when binding it on the home's
address without address reuse succeeds and nothing accepts a connection on
it there or on either loopback address. On macOS a bind with address reuse,
as the standard library makes, succeeds beside another program's wildcard
listener and takes its loopback traffic. A port observed free can still be
taken before the server listens; the server refuses to start beside a
listener that answers on its address, and startup reports the new holder. -/
def settle (port : PortSource) (served : Bool) : Holder → Settlement
  | .nobody | .thisHome => .keep
  | .anotherNode => if served || port == .chosen then .blockedByNode else .move
  | .anotherApp => if served || port == .chosen then .blockedByApp else .move

theorem a_chosen_port_never_moves (served : Bool) (holder : Holder) :
    settle .chosen served holder ≠ .move := by
  cases holder <;> cases served <;> decide

theorem a_home_that_has_served_never_moves (port : PortSource) (holder : Holder) :
    settle port true holder ≠ .move := by
  cases holder <;> cases port <;> decide

theorem this_homes_runtime_is_kept (port : PortSource) (served : Bool) :
    settle port served .thisHome = .keep := by
  cases port <;> cases served <;> rfl

theorem a_picked_port_is_picked_again_before_the_first_serve :
    settle .picked false .anotherApp = .move ∧ settle .picked false .anotherNode = .move := by
  decide

theorem moves_exactly_when_a_new_homes_unchosen_port_is_held
    (port : PortSource) (served : Bool) (holder : Holder) :
    settle port served holder = .move ↔
      served = false ∧ port ≠ .chosen ∧ (holder = .anotherNode ∨ holder = .anotherApp) := by
  cases holder <;> cases port <;> cases served <;> decide

def maxPort : Nat := 65535

/-- The ports a move tries, in order: the next hundred after the taken one
that fit a TCP port. Staying near the default keeps the port recognisable. -/
def candidates (taken : Nat) : List Nat :=
  ((List.range 100).map (taken + 1 + ·)).filter (· ≤ maxPort)

theorem a_candidate_is_a_usable_other_port {taken port : Nat} (h : port ∈ candidates taken) :
    port ≠ 0 ∧ port ≠ taken ∧ port ≤ maxPort := by
  simp only [candidates, List.mem_filter, List.mem_map, List.mem_range,
    decide_eq_true_eq] at h
  obtain ⟨⟨i, _, rfl⟩, hle⟩ := h
  omega

end NodePort
