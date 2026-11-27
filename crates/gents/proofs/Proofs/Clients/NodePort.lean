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

inductive Settlement where
  /-- Listen where the home already would. -/
  | keep
  /-- Record a free port in `network.json` and listen there. -/
  | move
  /-- Refuse, naming the other node and how to stop or move it. -/
  | blockedByNode
  /-- Refuse, naming the port the other app holds. -/
  | blockedByApp
  deriving DecidableEq, Repr

/-- Where a home's listener settles before its service is installed. `fresh`
is a home that has never served: it has no `runtime.json`, which only a
serving runtime writes.

Only a fresh home with no port recorded moves. Nothing has saved its address
yet, and clients learn the port it serves on from the `runtime.json` the
server writes once it listens. A home that has served, or whose port someone
chose, keeps it: saved connections and the person's own configuration name
that port, so the holder is reported instead. This home's own runtime is never
moved away from or launched over.

External premise: a port observed free can be taken before the server binds
it; the server then fails closed and startup reports the new holder. -/
def settle (recorded fresh : Bool) : Holder → Settlement
  | .nobody | .thisHome => .keep
  | .anotherNode => if fresh && !recorded then .move else .blockedByNode
  | .anotherApp => if fresh && !recorded then .move else .blockedByApp

theorem a_recorded_port_never_moves (fresh : Bool) (holder : Holder) :
    settle true fresh holder ≠ .move := by
  cases holder <;> cases fresh <;> decide

theorem a_home_that_has_served_never_moves (recorded : Bool) (holder : Holder) :
    settle recorded false holder ≠ .move := by
  cases holder <;> cases recorded <;> decide

theorem this_homes_runtime_is_kept (recorded fresh : Bool) :
    settle recorded fresh .thisHome = .keep := by
  cases recorded <;> cases fresh <;> rfl

theorem moves_exactly_when_a_new_homes_port_is_held (recorded fresh : Bool) (holder : Holder) :
    settle recorded fresh holder = .move ↔
      fresh = true ∧ recorded = false ∧ (holder = .anotherNode ∨ holder = .anotherApp) := by
  cases holder <;> cases recorded <;> cases fresh <;> decide

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
