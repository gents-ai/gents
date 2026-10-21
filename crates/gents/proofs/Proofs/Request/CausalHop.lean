import Proofs.Basic

namespace CausalHop

/-- No default loop cap. A configured cap still bounds admitted causal hops. -/
def defaultMaxRequestHop : Option Nat := none

inductive Cause where
  | root
  /-- Outward: caused by another session's action at hop `causeHop`
  (`agent_new`/`agent_message` request or steering continuation). -/
  | crossSession (causeHop : Nat)
  /-- Return: a session-message completion wake delivering a caused request's
  result back to the calling session. -/
  | returnEdge
  /-- A same-session continuation (retry, goal continuation, user steering,
  native process or plugin-job completion wake); cannot extend a chain. -/
  | continuation
  deriving DecidableEq, Repr

/-- Outward edges advance beyond their cause; returns preserve the caller's hop,
so sequential calls do not consume the cross-session bound. -/
def nextHop : Cause → Nat → Nat
  | .root, _ => 0
  | .crossSession causeHop, own => max own (causeHop + 1)
  | .returnEdge, own => own
  | .continuation, own => own

/-- Admission reads only the signed hop; it does not require predecessor
replication or sender cooperation. -/
def admitHop (maxHop : Option Nat) (hop : Nat) : Bool :=
  match maxHop with
  | none => true
  | some bound => decide (hop ≤ bound)

def sendTargetAllowed (callerSession targetSession : String) : Bool :=
  callerSession != targetSession

theorem root_hop_is_zero (own : Nat) : nextHop .root own = 0 := rfl

theorem continuation_preserves_hop (own : Nat) :
    nextHop .continuation own = own := rfl

/-- A plugin job's completion wakes its caller as a same-session continuation,
so the caller keeps its current hop. -/
theorem job_completion_wake_is_continuation (own : Nat) :
    nextHop .continuation own = own :=
  continuation_preserves_hop own

theorem return_keeps_caller_hop (own : Nat) : nextHop .returnEdge own = own := rfl

theorem return_into_refused_session_is_refused (maxHop own : Nat) (h : maxHop < own) :
    admitHop (some maxHop) (nextHop .returnEdge own) = false := by
  simp [nextHop, admitHop]; omega

theorem cross_session_exceeds_cause (causeHop own : Nat) :
    causeHop + 1 ≤ nextHop (.crossSession causeHop) own := by
  simp only [nextHop]; omega

theorem cross_session_keeps_own (causeHop own : Nat) :
    own ≤ nextHop (.crossSession causeHop) own := by
  simp only [nextHop]; omega

theorem no_cap_admits_any_hop (hop : Nat) : admitHop none hop = true := rfl

theorem configured_cap_admits_at_bound (bound : Nat) :
    admitHop (some bound) bound = true := by
  simp [admitHop]

theorem configured_cap_refuses_above_bound (bound hop : Nat) (h : bound < hop) :
    admitHop (some bound) hop = false := by
  simp [admitHop]; omega

theorem send_to_own_session_refused (session : String) :
    sendTargetAllowed session session = false := by
  simp [sendTargetAllowed]

inductive Step where
  | cross (ownPredecessorHop : Nat)
  | cont
  deriving DecidableEq, Repr

def Step.apply : Step → Nat → Nat
  | .cross own, hop => nextHop (.crossSession hop) own
  | .cont, hop => nextHop .continuation hop

def hopAlong (start : Nat) : List Step → Nat
  | [] => start
  | step :: rest => hopAlong (step.apply start) rest

def sends : List Step → Nat
  | [] => 0
  | .cross _ :: rest => sends rest + 1
  | .cont :: rest => sends rest

theorem hopAlong_ge (start : Nat) (steps : List Step) :
    start + sends steps ≤ hopAlong start steps := by
  induction steps generalizing start with
  | nil => simp [hopAlong, sends]
  | cons step rest ih =>
    cases step with
    | cross own =>
      have h := ih (Step.apply (.cross own) start)
      have h_step : start + 1 ≤ Step.apply (.cross own) start := by
        simp only [Step.apply, nextHop]; omega
      simp only [hopAlong, sends]
      omega
    | cont =>
      simpa [hopAlong, sends, Step.apply, nextHop] using ih start

theorem admitted_chain_sends_le_max (maxHop start : Nat) (steps : List Step)
    (h_admit : admitHop (some maxHop) (hopAlong start steps) = true) :
    sends steps ≤ maxHop := by
  have h := hopAlong_ge start steps
  simp [admitHop] at h_admit
  omega

theorem send_beyond_max_is_refused (maxHop start : Nat) (steps : List Step)
    (h_sends : maxHop < sends steps) :
    admitHop (some maxHop) (hopAlong start steps) = false := by
  have h := hopAlong_ge start steps
  simp [admitHop]
  omega

theorem continuation_preserves_admission (maxHop : Option Nat) (hop : Nat) :
    admitHop maxHop (nextHop .continuation hop) = admitHop maxHop hop := rfl

def pingPongHops (n : Nat) : List Nat :=
  (List.range n).foldl
    (fun hops _ =>
      let own := (hops.reverse.drop 1).head?.getD 0
      let cause := hops.getLast?.getD 0
      hops ++ [nextHop (.crossSession cause) own])
    [0]

structure Pair where
  a : Nat
  b : Nat
  deriving DecidableEq, Repr

inductive Event where
  | aSendsB
  | bSendsA
  | returnToA
  | returnToB
  deriving DecidableEq, Repr

def Event.isReturn : Event → Bool
  | .returnToA | .returnToB => true
  | _ => false

def Event.hop : Event → Pair → Nat
  | .aSendsB, p => nextHop (.crossSession p.a) p.b
  | .bSendsA, p => nextHop (.crossSession p.b) p.a
  | .returnToA, p => nextHop .returnEdge p.a
  | .returnToB, p => nextHop .returnEdge p.b

def Event.apply (e : Event) (p : Pair) : Pair :=
  match e with
  | .aSendsB | .returnToB => { p with b := e.hop p }
  | .bSendsA | .returnToA => { p with a := e.hop p }

def run (p : Pair) (events : List Event) : Pair := events.foldl (fun p e => e.apply p) p

theorem run_erases_returns (p : Pair) (events : List Event) :
    run p events = run p (events.filter (fun e => !e.isReturn)) := by
  induction events generalizing p with
  | nil => rfl
  | cons e rest ih =>
    cases e <;> simp [run, List.foldl, Event.isReturn, Event.apply, Event.hop, nextHop] at ih ⊢ <;>
      exact ih _

def sequentialCalls (n : Nat) : List Event :=
  (List.replicate n [Event.aSendsB, Event.returnToA]).flatten

/-- A caller making any number of sequential calls stays at hop zero and its
callee at hop one, so every call is admitted under any bound of at least one. -/
theorem sequential_calls_stay_at_depth_one (n : Nat) :
    run ⟨0, 1⟩ (sequentialCalls n) = ⟨0, 1⟩ ∧
      run ⟨0, 0⟩ (sequentialCalls (n + 1)) = ⟨0, 1⟩ := by
  have h : ∀ n, run ⟨0, 1⟩ (sequentialCalls n) = ⟨0, 1⟩ := by
    intro n
    induction n with
    | zero => rfl
    | succ k ih =>
      simp only [sequentialCalls, List.replicate_succ, List.flatten_cons] at ih ⊢
      simpa [run, List.foldl_append, Event.apply, Event.hop, nextHop] using ih
  refine ⟨h n, ?_⟩
  simpa [sequentialCalls, List.replicate_succ, List.flatten_cons, run, List.foldl_append,
    Event.apply, Event.hop, nextHop] using h n

/-! `created_at` has whole-second precision, so ties at the latest second must
use the highest hop. This errs toward refusing a continuation rather than
running one below a same-second refusal. -/

structure RequestStamp where
  second : Nat
  hop : Nat
  deriving DecidableEq, Repr

def latestSecond : List RequestStamp → Nat
  | [] => 0
  | r :: rs => max r.second (latestSecond rs)

def maxStampHop : List RequestStamp → Nat
  | [] => 0
  | r :: rs => max r.hop (maxStampHop rs)

theorem hop_le_maxStampHop {r : RequestStamp} :
    ∀ {rows : List RequestStamp}, r ∈ rows → r.hop ≤ maxStampHop rows
  | [], h => by simp at h
  | x :: xs, h => by
    simp only [List.mem_cons] at h
    simp only [maxStampHop]
    rcases h with h | h
    · subst h; omega
    · have := hop_le_maxStampHop h; omega

def sessionCurrentHop (rows : List RequestStamp) : Nat :=
  maxStampHop (rows.filter (fun r => r.second == latestSecond rows))

theorem latest_second_hop_le_current (rows : List RequestStamp) (r : RequestStamp)
    (h_mem : r ∈ rows) (h_latest : r.second = latestSecond rows) :
    r.hop ≤ sessionCurrentHop rows :=
  hop_le_maxStampHop (List.mem_filter.2 ⟨h_mem, by simp [h_latest]⟩)

theorem same_second_tie_takes_the_highest_hop :
    let rows := [⟨4, 0⟩, ⟨5, 9⟩, ⟨5, 3⟩]
    sessionCurrentHop rows = 9 ∧
      admitHop (some 8) (nextHop .continuation (sessionCurrentHop rows)) = false := by
  native_decide

end CausalHop
