import Proofs.Basic

/-!
# Causal hop: the only loop bound between agents

Every agent is an ordinary agent, addressed directly; the runtime encodes no
parent/child hierarchy. The one causal fact the runtime keeps is the hop: the
signed, immutable `AgentRequest.request_hop`, written once by
`lifecycle::materialize` and checked at admission against the target's
`Node.max_request_hop`.

Each session has a *current hop*: the hop of its latest request. The hop
bounds call *depth*, not how many calls a session makes, so it separates the
edges between sessions:

* An *outward* edge — a tool-caused new request or steering continuation of
  another session (`agent_new`, `agent_message`) — is strictly further than
  its cause: `hop = max current (cause + 1)`.
* A *return* edge — the completion wake that delivers a caused request's
  result back to the session that made the call, along `caused_by_parent_*` —
  keeps the caller's current hop. Counting it as `cause + 1` made every
  question-and-answer round trip climb two hops, so a caller exhausted the
  bound after a few sequential calls (#2065).
* Same-session continuations (retries, goal continuations, user steering,
  native process completion wakes) copy the session's current hop, never the
  hop of an older request that scheduled them.
* A user-authored or trigger root request is hop zero and so resets its
  session.

There is no separate refusal path: a request over the bound is written like
any other, becomes its session's latest request, and is refused at admission,
so every later return or same-session continuation copies a hop over the
bound and is refused too; the session waits for its user. Return edges never
change a hop, so a loop between agents — two sessions messaging each other —
still climbs by at least one per send and is refused after a bounded number of
sends, with no cascade, fence or tree walk. The hop deliberately does not
bound how many times a caller acts on its own returned results. An
`agent_message` to the caller's own current session is refused outright: it
would be a same-session steering continuation with no hop increase.
-/

namespace CausalHop

/-- Default `Node.max_request_hop` when the node leaves it unset. -/
def defaultMaxRequestHop : Nat := 8

/-- Why a request exists, relative to the request whose hop it inherits. -/
inductive Cause where
  /-- A user, trigger or schedule root. -/
  | root
  /-- Outward: caused by another session's action at hop `causeHop`, a
  `agent_new`/`agent_message` request or steering continuation whose cause is
  the calling request. -/
  | crossSession (causeHop : Nat)
  /-- Return: a session-message completion wake delivering a caused request's
  result back to the calling session. -/
  | returnEdge
  /-- A same-session continuation: retry, goal continuation, user steering or
  a native process completion wake. It cannot extend a chain. -/
  | continuation
  deriving DecidableEq, Repr

/-- The hop a request is materialized with, given its session's current hop
(the hop of its latest request; `0` for a new session). -/
def nextHop : Cause → Nat → Nat
  | .root, _ => 0
  | .crossSession causeHop, own => max own (causeHop + 1)
  | .returnEdge, own => own
  | .continuation, own => own

/-- Admission refuses a request whose hop exceeds the target node's
`max_request_hop`. The check reads only the signed hop; no lineage walk,
replication of the predecessor, or cooperation of the sender is needed. -/
def admitHop (maxHop hop : Nat) : Bool := decide (hop ≤ maxHop)

/-- `agent_message` never addresses the caller's own current session. -/
def sendTargetAllowed (callerSession targetSession : String) : Bool :=
  callerSession != targetSession

theorem root_hop_is_zero (own : Nat) : nextHop .root own = 0 := rfl

theorem continuation_preserves_hop (own : Nat) :
    nextHop .continuation own = own := rfl

/-- A returned result keeps the caller's current hop, whatever the hop of the
caused request that produced it. -/
theorem return_keeps_caller_hop (own : Nat) : nextHop .returnEdge own = own := rfl

/-- A return into a session whose current hop is over the bound is refused,
like every other continuation of that session. -/
theorem return_into_refused_session_is_refused (maxHop own : Nat) (h : maxHop < own) :
    admitHop maxHop (nextHop .returnEdge own) = false := by
  simp [nextHop, admitHop]; omega

/-- A cross-session cause is strictly behind what it causes, whatever the
target session's own history. -/
theorem cross_session_exceeds_cause (causeHop own : Nat) :
    causeHop + 1 ≤ nextHop (.crossSession causeHop) own := by
  simp only [nextHop]; omega

/-- A cross-session cause never lowers the target session's own hop. -/
theorem cross_session_keeps_own (causeHop own : Nat) :
    own ≤ nextHop (.crossSession causeHop) own := by
  simp only [nextHop]; omega

theorem send_to_own_session_refused (session : String) :
    sendTargetAllowed session session = false := by
  simp [sendTargetAllowed]

/-- One link of a causal chain across sessions: the next request is either
caused by the current one from another session (with its own session's
predecessor hop), or continues it in the same session. -/
inductive Step where
  | cross (ownPredecessorHop : Nat)
  | cont
  deriving DecidableEq, Repr

def Step.apply : Step → Nat → Nat
  | .cross own, hop => nextHop (.crossSession hop) own
  | .cont, hop => nextHop .continuation hop

/-- The hop reached after a chain of steps from a starting hop. -/
def hopAlong (start : Nat) : List Step → Nat
  | [] => start
  | step :: rest => hopAlong (step.apply start) rest

/-- Number of agent-to-agent sends in a chain. -/
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

/-- Every admitted request has at most `maxHop` agent-to-agent sends in its
causal chain, across any sessions and interleaved continuations: `maxHop` is
the `max_request_hop` of the node that admits it. There is no global
constant; `defaultMaxRequestHop` only fills an unset field. -/
theorem admitted_chain_sends_le_max (maxHop start : Nat) (steps : List Step)
    (h_admit : admitHop maxHop (hopAlong start steps) = true) :
    sends steps ≤ maxHop := by
  have h := hopAlong_ge start steps
  simp [admitHop] at h_admit
  omega

/-- A message loop is cut: once a chain holds more sends than the admitting
target's bound, its request is refused. -/
theorem send_beyond_max_is_refused (maxHop start : Nat) (steps : List Step)
    (h_sends : maxHop < sends steps) :
    admitHop maxHop (hopAlong start steps) = false := by
  have h := hopAlong_ge start steps
  simp [admitHop]
  omega

/-- Continuations never change admissibility: retries, goal continuations,
user steering and native completion wakes of an admitted request stay
admitted. -/
theorem continuation_preserves_admission (maxHop hop : Nat) :
    admitHop maxHop (nextHop .continuation hop) = admitHop maxHop hop := rfl

/-- Two sessions messaging each other from a root at hop zero: each send
continues the other session, whose current hop is that of the request two
links back. Returns do not appear: they change no hop (`run_erases_returns`). -/
def pingPongHops (n : Nat) : List Nat :=
  (List.range n).foldl
    (fun hops _ =>
      let own := (hops.reverse.drop 1).head?.getD 0
      let cause := hops.getLast?.getD 0
      hops ++ [nextHop (.crossSession cause) own])
    [0]

/-! ## Calls and returns between two sessions

A caller `a` and a callee `b`, each at its current hop. An outward send from
one session is materialized in the other; a return is materialized in the
session that made the call. -/

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

/-- The hop of the request an event materializes. -/
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

/-- Returns are invisible to the hop: a trace with its returns erased reaches
the same hops, so interleaving results never lowers or raises a chain. -/
theorem run_erases_returns (p : Pair) (events : List Event) :
    run p events = run p (events.filter (fun e => !e.isReturn)) := by
  induction events generalizing p with
  | nil => rfl
  | cons e rest ih =>
    cases e <;> simp [run, List.foldl, Event.isReturn, Event.apply, Event.hop, nextHop] at ih ⊢ <;>
      exact ih _

/-- `n` sequential calls from a root caller to one callee, each followed by
its returned result. -/
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

/-! ## The session current hop

`created_at` has whole-second precision, so several requests can share the
latest second with no order between them. The current hop is the highest hop
among the requests of the latest second: it errs toward refusing a
continuation, never toward running one below a refusal. A user root written in
the same second as a refused wake therefore still reads as refused until the
next user message. -/

/-- A request as the hop owner sees it: its whole-second creation time and
its hop. -/
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

/-- The session's current hop: the highest hop among its latest second. -/
def sessionCurrentHop (rows : List RequestStamp) : Nat :=
  maxStampHop (rows.filter (fun r => r.second == latestSecond rows))

/-- No request of the latest second is above the current hop, so a tie can
never select a lower hop than a refusal written in the same second. -/
theorem latest_second_hop_le_current (rows : List RequestStamp) (r : RequestStamp)
    (h_mem : r ∈ rows) (h_latest : r.second = latestSecond rows) :
    r.hop ≤ sessionCurrentHop rows :=
  hop_le_maxStampHop (List.mem_filter.2 ⟨h_mem, by simp [h_latest]⟩)

/-- A refused max+1 wake and a native wake written in the same second read as
the refused hop, and a continuation copying it is refused. -/
theorem same_second_tie_takes_the_highest_hop :
    let rows := [⟨4, 0⟩, ⟨5, defaultMaxRequestHop + 1⟩, ⟨5, 3⟩]
    sessionCurrentHop rows = defaultMaxRequestHop + 1 ∧
      admitHop defaultMaxRequestHop (nextHop .continuation (sessionCurrentHop rows)) =
        false := by
  native_decide

end CausalHop
