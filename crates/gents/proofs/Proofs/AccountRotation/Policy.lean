import Proofs.Basic

/-!
# Claude subscription account rotation

A node may hold several Claude subscription accounts. Every model call selects
one of them from each account's last observed limit state, the requested model
family, the account the session is on and a round-robin cursor.

External premise (Claude Code 2.1.311 rate-limit simulator, `chunk-v2tvd4xz.js`
`setScenario`/`setEarlyWarning`/`setOverageScenarioHeaders`): every Messages
response reports unified limits in `anthropic-ratelimit-unified-*` headers.
`status` is `allowed`, `allowed_warning` (a window passed its early-warning
threshold) or `rejected`; `representative-claim` names the binding window
(`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`); `reset` carries
epoch seconds. A rejected subscription keeps serving through paid extra usage
while `overage-status` is `allowed` or `allowed_warning`.

The policy prefers an account with no live limit, then one past its early
warning, then (only when the operator enabled it) one drawing paid overage. It
keeps a session on its account while that account is in the best available tier,
so the provider cache stays warm, and otherwise moves to the next account of
that tier in round-robin order.
-/

namespace AccountRotation

inductive Family where
  | opus
  | sonnet
  | other
  deriving DecidableEq, Repr

inductive Claim where
  | fiveHour
  | sevenDay
  | sevenDayOpus
  | sevenDaySonnet
  /-- A claim this client does not recognise binds every family. -/
  | unknown
  deriving DecidableEq, Repr

def Claim.binds : Claim → Family → Bool
  | .sevenDayOpus, family => family == .opus
  | .sevenDaySonnet, family => family == .sonnet
  | _, _ => true

inductive Severity where
  | warning
  | rejected
  deriving DecidableEq, Repr

structure Limit where
  claim : Claim
  severity : Severity
  resetAt : Time
  deriving DecidableEq, Repr

inductive Overage where
  | absent
  | allowed
  | warning
  | rejected
  deriving DecidableEq, Repr

def Overage.serves : Overage → Bool
  | .allowed | .warning => true
  | .absent | .rejected => false

structure Account where
  enabled : Bool
  limits : List Limit
  overage : Overage
  /-- A provider rejection blocks the account until this time even when the
  rejection carried no reset header. -/
  blockedUntil : Time
  deriving DecidableEq, Repr

inductive Tier where
  | clear
  | warned
  | overage
  deriving DecidableEq, Repr

def Tier.rank : Tier → Nat
  | .clear => 0
  | .warned => 1
  | .overage => 2

structure Policy where
  /-- Paid extra usage is an operator choice and is off by default. -/
  allowOverage : Bool := false
  deriving DecidableEq, Repr

def Limit.live (now : Time) (family : Family) (limit : Limit) : Bool :=
  decide (now < limit.resetAt) && limit.claim.binds family

def liveLimits (now : Time) (family : Family) (account : Account) : List Limit :=
  account.limits.filter (Limit.live now family)

def tier? (policy : Policy) (now : Time) (family : Family) (account : Account) :
    Option Tier :=
  if !account.enabled || decide (now < account.blockedUntil) then none
  else if (liveLimits now family account).any (·.severity == .rejected) then
    if policy.allowOverage && account.overage.serves then some .overage else none
  else if (liveLimits now family account).any (·.severity == .warning) then some .warned
  else some .clear

def ranks (policy : Policy) (now : Time) (family : Family) (pool : List Account) :
    List (Option Nat) :=
  pool.map fun account => (tier? policy now family account).map Tier.rank

def bestRank? : List (Option Nat) → Option Nat
  | [] => none
  | none :: rest => bestRank? rest
  | some rank :: rest =>
    match bestRank? rest with
    | none => some rank
    | some best => some (min rank best)

/-- Round-robin order from `start`: `start, …, n-1, 0, …, start-1`. -/
def cyclic (n start : Nat) : List Nat :=
  (List.range n).filter (fun i => decide (start ≤ i)) ++
    (List.range n).filter (fun i => decide (i < start))

def hit (rs : List (Option Nat)) (best i : Nat) : Bool :=
  decide (rs[i]? = some (some best))

def pick (rs : List (Option Nat)) (n : Nat) (current : Option Nat) (cursor : Nat) :
    Option Nat :=
  match bestRank? rs with
  | none => none
  | some best =>
    match current with
    | some c => if hit rs best c then some c else (cyclic n (c + 1)).find? (hit rs best)
    | none => (cyclic n cursor).find? (hit rs best)

/-- The account for the next model call. `current` is the session's account;
`cursor` places a session that has none yet. -/
def select (policy : Policy) (now : Time) (family : Family) (pool : List Account)
    (current : Option Nat) (cursor : Nat) : Option Nat :=
  pick (ranks policy now family pool) pool.length current cursor

/-- Fold one response's limit report into the account. The report supersedes
every recorded limit that binds the request's family; limits binding only another
family are kept. -/
def observe (family : Family) (reported : List Limit) (overage : Overage)
    (account : Account) : Account :=
  { account with
    limits := account.limits.filter (fun limit => !limit.claim.binds family) ++ reported
    overage := overage }

/-- A provider rejection blocks the account until the later of its reset and the
next tick. -/
def reject (now resetAt : Time) (account : Account) : Account :=
  { account with blockedUntil := max account.blockedUntil (max resetAt (now + 1)) }

def markRejected (now : Time) (pool : List Account) (i : Nat) (resetAt : Time) :
    List Account :=
  match pool[i]? with
  | none => pool
  | some account => pool.set i (reject now resetAt account)

/-- The sends of one provider attempt. `rejects i` is the provider's verdict for
account `i`: `some reset` refuses it, `none` serves the request. A refusal moves
the attempt to the next selection without backoff. -/
def attempt (policy : Policy) (now : Time) (family : Family) (rejects : Nat → Option Time) :
    Nat → List Account → Option Nat → Nat → List Nat
  | 0, _, _, _ => []
  | fuel + 1, pool, current, cursor =>
    match select policy now family pool current cursor with
    | none => []
    | some i =>
      match rejects i with
      | none => [i]
      | some resetAt =>
        i :: attempt policy now family rejects fuel (markRejected now pool i resetAt)
          (some i) cursor

end AccountRotation
