---- MODULE ToolDeadlineInheritance ----
\* lean: none
\* crates: gents-session, gents-toolcalls
\* models: ComposedState.coherent_tool_deadline_eq_request_deadline
(***************************************************************************)
(* Where an accepted tool call's deadline comes from, and what a test may  *)
(* assert about it.                                                         *)
(*                                                                          *)
(* A request has one deadline. The claim fixes it                          *)
(* (`RequestLifecycle::claim_at`, the request's own `deadline` or           *)
(* `now + deadline_duration_secs`) and the lifecycle keeps it as            *)
(* `claimed_deadline_at`. Two copies follow:                                *)
(*                                                                          *)
(*  - the durable one: the provider-turn publication writes each accepted   *)
(*    `AgentToolCall` with `deadline_at = claimed_deadline_at`              *)
(*    (`DefraStreamWriter` accepted-tool plan, `canonical/publish.rs`), and *)
(*    the dispatch start (`start_running_in_txn`) never rewrites it;        *)
(*  - the in-memory one: the session hook's `request_deadline_at`, which    *)
(*    `ensure_assistant_turn_sequence` hands to `adopt_accepted_tool_       *)
(*    dispatch` as the dispatched lifecycle's `deadline_at`. The runtime    *)
(*    binds it from the same claim (`prepare_inference`:                    *)
(*    `set_request_deadline_at(lifecycle.claimed_deadline_at())`).          *)
(*                                                                          *)
(* `Agree`: the durable and in-memory deadlines of a dispatched tool are    *)
(* equal. It holds whenever the hook's deadline is bound from the claim     *)
(* (`FromClaim`), under every schedule and clock. A test fixture that binds *)
(* the hook from its own wall-clock reading instead (`FromClaim = FALSE`)   *)
(* gives one request two deadlines.                                         *)
(*                                                                          *)
(* `TestPasses` is what `hook_attaches_active_request_deadline_to_tool_     *)
(* call_lifecycle` asserts after dispatch: the row's deadline is the        *)
(* deadline the test bound (`<=` it before this spec, `=` it now) and lies  *)
(* ahead of the clock.                                                      *)
(*                                                                          *)
(* GREEN `MCToolDeadlineInheritance`: bound from the claim, unreached       *)
(* claim deadline; both hold under every delay. RED                         *)
(* `MCToolDeadlineInheritanceShortClaim` is the fixture before #3840        *)
(* (claim + 60 s, hook now + 300 s): the row carries the shorter claim     *)
(* deadline, so `Agree` fails on every schedule and the test's `<=` held    *)
(* only by the ratio, failing `> now` on a host slower than the claim       *)
(* deadline. RED `MCToolDeadlineInheritanceLongClaim` is the fixture after  *)
(* #3840 (claim + 2400 s, hook now + 300 s): with no scheduling delay at    *)
(* all the row's deadline exceeds the test's, so the test fails on every    *)
(* run; it was never an interleaving.                                       *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  ClaimTtl,   \* ticks from the claim to the claim's request deadline
  BindTtl,    \* ticks from the bind to a fixture's own wall-clock deadline
  FromClaim,  \* the hook's deadline is bound from the claim
  Exact,      \* the test asserts `row = bound` (TRUE) or `row <= bound`
  MaxDelay,   \* ticks the host may hold any step
  Horizon     \* clock bound for model checking

VARIABLES
  clock,       \* wall clock in ticks
  stall,       \* ticks the current step has waited
  pc,          \* "claim" | "bind" | "publish" | "dispatch" | "read" | "done"
  claimed,     \* RequestLifecycle.claimed_deadline_at
  hook,        \* DefraSessionHook request_deadline_at (and the test's bound)
  row,         \* AgentToolCall.deadline_at
  memory,      \* dispatched ToolCallLifecycle.deadline_at
  verdict      \* "running" | "pass" | "fail"

vars == <<clock, stall, pc, claimed, hook, row, memory, verdict>>

None == 0

Init ==
  /\ clock = 0 /\ stall = 0 /\ pc = "claim"
  /\ claimed = None /\ hook = None /\ row = None /\ memory = None
  /\ verdict = "running"

Tick ==
  /\ pc # "done" /\ clock < Horizon /\ stall < MaxDelay
  /\ clock' = clock + 1 /\ stall' = stall + 1
  /\ UNCHANGED <<pc, claimed, hook, row, memory, verdict>>

Step(from, to) == pc = from /\ pc' = to /\ stall' = 0

Claim ==
  /\ Step("claim", "bind")
  /\ claimed' = clock + ClaimTtl
  /\ UNCHANGED <<clock, hook, row, memory, verdict>>

Bind ==
  /\ Step("bind", "publish")
  /\ hook' = IF FromClaim THEN claimed ELSE clock + BindTtl
  /\ UNCHANGED <<clock, claimed, row, memory, verdict>>

(* The publication is fenced on the claim's deadline; past it the product *)
(* correctly refuses and the scenario ends without a row.                  *)
Publish ==
  /\ Step("publish", IF clock < claimed THEN "dispatch" ELSE "done")
  /\ row' = IF clock < claimed THEN claimed ELSE None
  /\ verdict' = IF clock < claimed THEN verdict ELSE "fail"
  /\ UNCHANGED <<clock, claimed, hook, memory>>

Dispatch ==
  /\ Step("dispatch", "read")
  /\ memory' = hook
  /\ UNCHANGED <<clock, claimed, hook, row, verdict>>

Read ==
  /\ Step("read", "done")
  /\ verdict' = IF (IF Exact THEN row = hook ELSE row <= hook) /\ row > clock
                 THEN "pass" ELSE "fail"
  /\ UNCHANGED <<clock, claimed, hook, row, memory>>

Next == Tick \/ Claim \/ Bind \/ Publish \/ Dispatch \/ Read

Spec == Init /\ [][Next]_vars /\ WF_vars(Claim \/ Bind \/ Publish \/ Dispatch \/ Read)

TypeOK ==
  /\ clock \in 0..Horizon
  /\ stall \in 0..MaxDelay
  /\ pc \in {"claim", "bind", "publish", "dispatch", "read", "done"}
  /\ verdict \in {"running", "pass", "fail"}

Agree == pc \in {"read", "done"} /\ memory # None => memory = row

TestPasses == verdict # "fail"

Settles == <>(pc = "done")

====
