namespace ToolPolicy.ParallelTools

/-- Paths are canonical, root-scoped components supplied by the owning tool.
The footprint must cover aliases as well as descendants; path normalization
alone does not establish that two hard links are disjoint. An unresolved or
incomplete footprint, an unknown effect, or a hook whose decisions depend on
prior results must be a barrier. -/
abbrev Path := List String

inductive Footprint where
  | read (paths : List Path)
  | write (paths : List Path)
  | barrier
  deriving DecidableEq, Repr

private def pathPrefix : Path → Path → Bool
  | [], _ => true
  | _ :: _, [] => false
  | x :: xs, y :: ys => x == y && pathPrefix xs ys

def overlaps (a b : Path) : Bool := pathPrefix a b || pathPrefix b a

theorem overlaps_symmetric (a b : Path) : overlaps a b = overlaps b a := by
  simp [overlaps, Bool.or_comm]

def intersects (as bs : List Path) : Bool :=
  as.any fun a => bs.any fun b => overlaps a b

/-- Read-only calls can overlap. Every write must be disjoint from the other
call's complete footprint; unknown effects remain serial. -/
def conflicts : Footprint → Footprint → Bool
  | .barrier, _ | _, .barrier => true
  | .read _, .read _ => false
  | .read a, .write b | .write a, .read b | .write a, .write b => intersects a b

structure Call where
  id : Nat
  footprint : Footprint
  deriving DecidableEq, Repr

def compatible (wave : List Call) (call : Call) : Bool :=
  wave.all fun prior => !conflicts prior.footprint call.footprint

/-- A wave is a consecutive prefix. Stopping at the first conflict prevents a
later call from overtaking an intervening mutation or an unknown effect. -/
def extendWave (wave : List Call) : List Call → List Call × List Call
  | [] => (wave, [])
  | call :: rest =>
      if compatible wave call then extendWave (wave ++ [call]) rest
      else (wave, call :: rest)

def nextWave : List Call → List Call × List Call
  | [] => ([], [])
  | call :: rest =>
      if call.footprint == .barrier then ([call], rest)
      else extendWave [call] rest

/-- Results are projected in admission order, independently of completion
order. A missing result prevents publishing the incomplete wave. -/
def orderedResults (calls : List Call) (result : Nat → Option String) : Option (List String) :=
  calls.mapM fun call => result call.id

inductive Decision where
  | run | skip | terminate
  deriving DecidableEq, Repr

/-- Admission ends at Terminate; Skip never starts a call. Previously started
calls must be cancelled and terminalized by the existing lifecycle owner. -/
def admitted : List (Call × Decision) → List Call
  | [] => []
  | (call, .run) :: tail => call :: admitted tail
  | (_, .skip) :: tail => admitted tail
  | (_, .terminate) :: _ => []

theorem barrier_runs_alone (id : Nat) (rest : List Call) :
    nextWave ({ id := id, footprint := .barrier } :: rest) =
      ([{ id := id, footprint := .barrier }], rest) := by
  simp [nextWave]

theorem termination_stops_admission (call : Call) (rest : List (Call × Decision)) :
    admitted ((call, .terminate) :: rest) = [] := by rfl

theorem skipped_call_is_not_started (call : Call) (rest : List (Call × Decision)) :
    admitted ((call, .skip) :: rest) = admitted rest := by rfl

theorem extend_wave_preserves_order (wave rest : List Call) :
    (extendWave wave rest).1 ++ (extendWave wave rest).2 = wave ++ rest := by
  induction rest generalizing wave with
  | nil => simp [extendWave]
  | cons call rest ih =>
    simp only [extendWave]
    split
    · simpa [List.append_assoc] using ih (wave ++ [call])
    · simp

inductive Execution where
  | running | cancelRequested | finished (value : String) | failed (reason : String)
  | skipped (reason : String) | cancelled
  deriving DecidableEq, Repr

/-- Stop requests cancellation of every running call. Its durable terminal is
still owned by the execution lifecycle; requesting cancellation is not a
terminal result. Already settled and skipped calls remain settled. -/
def stop : Execution → Execution
  | .running => .cancelRequested
  | other => other

def stopWave (wave : List Execution) : List Execution := wave.map stop

theorem stop_leaves_no_running (wave : List Execution) :
    Execution.running ∉ stopWave wave := by
  simp only [stopWave, List.mem_map, not_exists]
  intro execution ⟨_, h⟩
  cases execution <;> simp [stop] at h

theorem stop_preserves_settled (value : String) : stop (.finished value) = .finished value := by rfl

theorem stop_preserves_failure (reason : String) : stop (.failed reason) = .failed reason := by rfl

end ToolPolicy.ParallelTools
