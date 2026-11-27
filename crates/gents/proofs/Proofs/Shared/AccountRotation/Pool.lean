import Proofs.Shared.AccountRotation.Properties

/-!
# Runtime AccountPool decisions and invariants

Models the pure state decisions of `gents_claude::pool::AccountPool` and the
post-rejection rotation check of `open_rotated`:

- Round-robin cursor advancement (`cursorAction`, `nextCursor`): advances past
  the chosen account only when the session has no current account in the active
  list.
- Session stickiness map maintenance (`capStatus`, `sessionMapAction`): keeps
  the map unchanged when the session stays on its account, retains no entry when
  `sessionCap = 0`, and clears the map before inserting an unrecorded session
  at or above `sessionCap` so the map never exceeds its cap.
- Earliest reset calculation (`accountResetAt`, `earliestReset`): computes the
  exact earliest time at or after `now` at which some listed account can serve
  `family` under `policy` (accounting for paid overage bypassing rejected
  subscription limits while still respecting `blockedUntil`).
- Re-selection rotation marking (`markRotatedOnFailure`): annotates a failed
  provider open as `account-rotated` only when the provider rejected the call
  and a follow-up `choose` yields a different eligible account.
-/

namespace AccountRotation

inductive CursorAction where
  | keep
  | advancePastChosen
  deriving DecidableEq, Repr

/-- `AccountPool.choose` advances the round-robin cursor only when the session
had no current account in the active account list. -/
def cursorAction (hasCurrent : Bool) : CursorAction :=
  if hasCurrent then .keep else .advancePastChosen

def nextCursor (current : Option Nat) (chosen cursor : Nat) : Nat :=
  match cursorAction current.isSome with
  | .keep => cursor
  | .advancePastChosen => chosen + 1

inductive CapStatus where
  | zero
  | belowCap
  | atOrAboveCap
  deriving DecidableEq, Repr

inductive SessionMapAction where
  | keep
  | insert
  | clearAndInsert
  | clearOnly
  deriving DecidableEq, Repr

def capStatus (len cap : Nat) : CapStatus :=
  if cap == 0 then .zero
  else if len < cap then .belowCap
  else .atOrAboveCap

/-- How `AccountPool.choose` updates its session stickiness map after selecting
an account. Clearing the map loses only stickiness and never touches account
limits. -/
def sessionMapAction (switched recorded : Bool) (cap : CapStatus) : SessionMapAction :=
  if !switched then .keep
  else match cap with
    | .zero => .clearOnly
    | .atOrAboveCap => if recorded then .insert else .clearAndInsert
    | .belowCap => .insert

inductive Reselection where
  | exhausted
  | sameAccount
  | differentAccount
  deriving DecidableEq, Repr

/-- Whether a failed `open_rotated` call marks its error `account-rotated`: only
when the provider rejected the call and a follow-up `choose` picks a different
account. -/
def markRotatedOnFailure (rejected : Bool) (reselection : Reselection) : Bool :=
  rejected && reselection == .differentAccount

def freshAccount : Account :=
  { enabled := true, limits := [], overage := .absent, blockedUntil := 0 }

def isBlockingLimit (now : Time) (family : Family) (limit : Limit) : Bool :=
  limit.severity == .rejected && decide (now < limit.resetAt) && limit.claim.binds family

def maxBlockingReset (now : Time) (family : Family) (limits : List Limit) (acc : Time) : Time :=
  limits.foldl
    (fun cur limit => if isBlockingLimit now family limit then max cur limit.resetAt else cur)
    acc

/-- Soonest time at or after `now` when an enabled `account` can serve `family`
under `policy`. Paid overage serves through rejected subscription limits, so
only `blockedUntil` holds back an overage-capable account. -/
def accountResetAt (policy : Policy) (now : Time) (family : Family) (account : Account) : Time :=
  let base := max now account.blockedUntil
  if policy.allowOverage && account.overage.serves then base
  else maxBlockingReset now family account.limits base

/-- Soonest time any listed account (treated as enabled, as `AccountPool` does)
can serve `family` under `policy`, or `none` when `accounts` is empty. -/
def earliestReset (policy : Policy) (now : Time) (family : Family) :
    List Account → Option Time
  | [] => none
  | account :: rest =>
    let ready := accountResetAt policy now family { account with enabled := true }
    match earliestReset policy now family rest with
    | none => some ready
    | some best => some (min ready best)

def mapGet {α : Type} (m : List (String × α)) (k : String) : Option α :=
  (m.find? (fun p => p.1 == k)).map Prod.snd

def mapSet {α : Type} (m : List (String × α)) (k : String) (v : α) : List (String × α) :=
  match m with
  | [] => [(k, v)]
  | (k', v') :: rest =>
    if k' == k then (k, v) :: rest
    else (k', v') :: mapSet rest k v

def mapRemove {α : Type} (m : List (String × α)) (k : String) : List (String × α) :=
  m.filter (fun p => p.1 != k)

def applySessionMapAction (sessions : List (String × String)) (session key : String)
    (action : SessionMapAction) : List (String × String) :=
  match action with
  | .keep => sessions
  | .insert => mapSet sessions session key
  | .clearAndInsert => [(session, key)]
  | .clearOnly => []

structure PoolState where
  limits : List (String × Account) := []
  sessions : List (String × String) := []
  cursor : Nat := 0
  sessionCap : Nat
  deriving DecidableEq, Repr

structure PoolChoice where
  key : String
  index : Nat
  switched : Bool
  deriving DecidableEq, Repr

def PoolState.snapshot (pool : PoolState) (key : String) : Account :=
  { (mapGet pool.limits key).getD freshAccount with enabled := true }

def PoolState.updateAccount (pool : PoolState) (key : String) (f : Account → Account) : PoolState :=
  let next := f (pool.snapshot key)
  { pool with limits := mapSet pool.limits key next }

def PoolState.observe (pool : PoolState) (key : String) (family : Family)
    (reported : List Limit) (overage : Overage) : PoolState :=
  pool.updateAccount key (AccountRotation.observe family reported overage)

def PoolState.reject (pool : PoolState) (key : String) (now resetAt : Time) : PoolState :=
  pool.updateAccount key (AccountRotation.reject now resetAt)

def PoolState.forgetAccount (pool : PoolState) (key : String) : PoolState :=
  { pool with limits := mapRemove pool.limits key }

def PoolState.forgetSession (pool : PoolState) (session : String) : PoolState :=
  { pool with sessions := mapRemove pool.sessions session }

def PoolState.earliestReset (pool : PoolState) (accounts : List String)
    (family : Family) (now : Time) (policy : Policy) : Option Time :=
  AccountRotation.earliestReset policy now family (accounts.map pool.snapshot)

def PoolState.choose (pool : PoolState) (accounts : List String) (session : String)
    (family : Family) (now : Time) (policy : Policy) : PoolState × Option PoolChoice :=
  let list := accounts.map pool.snapshot
  let recorded := mapGet pool.sessions session
  let current := recorded.bind (fun key => accounts.idxOf? key)
  match select policy now family list current pool.cursor with
  | none => (pool, none)
  | some index =>
    match accounts[index]? with
    | none => (pool, none)
    | some key =>
      let cursor' := nextCursor current index pool.cursor
      let switched := decide (current ≠ some index)
      let cap := capStatus pool.sessions.length pool.sessionCap
      let action := sessionMapAction switched recorded.isSome cap
      let sessions' := applySessionMapAction pool.sessions session key action
      ({ pool with cursor := cursor', sessions := sessions' },
        some { key, index, switched })

/-! ## Theorems -/

theorem nextCursor_advances_without_current (chosen cursor : Nat) :
    nextCursor none chosen cursor = chosen + 1 := rfl

theorem nextCursor_keeps_with_current (c chosen cursor : Nat) :
    nextCursor (some c) chosen cursor = cursor := rfl

theorem mapGet_mapSet_self {α : Type} (m : List (String × α)) (k : String) (v : α) :
    mapGet (mapSet m k v) k = some v := by
  induction m with
  | nil => simp [mapSet, mapGet]
  | cons hd rest ih =>
    rcases hd with ⟨k', v'⟩
    by_cases h : k' = k
    · subst h
      simp [mapSet, mapGet]
    · simp [mapSet, mapGet, h]
      simpa [mapGet] using ih

theorem mapSet_length_of_isSome {α : Type} {m : List (String × α)} {k : String} (v : α)
    (h : (mapGet m k).isSome = true) : (mapSet m k v).length = m.length := by
  induction m with
  | nil => simp [mapGet] at h
  | cons hd rest ih =>
    rcases hd with ⟨k', v'⟩
    by_cases heq : k' = k
    · subst heq
      simp [mapSet]
    · simp only [mapSet, beq_iff_eq, heq, ↓reduceIte, List.length_cons]
      have hrest : (mapGet rest k).isSome = true := by
        simpa [mapGet, heq] using h
      rw [ih hrest]

theorem mapSet_length_of_none {α : Type} {m : List (String × α)} {k : String} (v : α)
    (h : mapGet m k = none) : (mapSet m k v).length = m.length + 1 := by
  induction m with
  | nil => simp [mapSet]
  | cons hd rest ih =>
    rcases hd with ⟨k', v'⟩
    by_cases heq : k' = k
    · subst heq
      simp [mapGet] at h
    · simp only [mapSet, beq_iff_eq, heq, ↓reduceIte, List.length_cons]
      have hrest : mapGet rest k = none := by
        simpa [mapGet, heq] using h
      rw [ih hrest]

/-- Applying `sessionMapAction` never allows the session map to exceed
`sessionCap`, including when `sessionCap = 0`. -/
theorem applySessionMapAction_preserves_cap (sessions : List (String × String))
    (session key : String) (switched : Bool) (cap : Nat)
    (hlen : sessions.length ≤ cap) :
    (applySessionMapAction sessions session key
      (sessionMapAction switched (mapGet sessions session).isSome
        (capStatus sessions.length cap))).length ≤ cap := by
  cases switched with
  | false =>
    simpa [sessionMapAction, applySessionMapAction] using hlen
  | true =>
    unfold sessionMapAction capStatus
    by_cases hzero : cap = 0
    · simp [hzero, applySessionMapAction]
    · by_cases hlt : sessions.length < cap
      · simp only [Bool.not_true, Bool.false_eq_true, ↓reduceIte, beq_iff_eq, hzero, hlt,
          applySessionMapAction]
        cases hget : mapGet sessions session with
        | none =>
          rw [mapSet_length_of_none key hget]
          omega
        | some existing =>
          rw [mapSet_length_of_isSome key (by simp [hget])]
          exact hlen
      · simp only [Bool.not_true, Bool.false_eq_true, ↓reduceIte, beq_iff_eq, hzero, hlt]
        cases hget : mapGet sessions session with
        | none =>
          simp only [Option.isSome_none, Bool.false_eq_true, ↓reduceIte, applySessionMapAction,
            List.length_singleton]
          omega
        | some existing =>
          simp only [Option.isSome_some, ↓reduceIte, applySessionMapAction]
          rw [mapSet_length_of_isSome key (by simp [hget])]
          exact hlen

/-- `PoolState.choose` preserves the session-cap bound across every call. -/
theorem poolChoose_preserves_cap {pool : PoolState} {accounts : List String}
    {session : String} {family : Family} {now : Time} {policy : Policy}
    (hlen : pool.sessions.length ≤ pool.sessionCap) :
    (pool.choose accounts session family now policy).1.sessions.length ≤ pool.sessionCap := by
  unfold PoolState.choose
  dsimp only
  cases hs : select policy now family (accounts.map pool.snapshot)
      ((mapGet pool.sessions session).bind fun key => accounts.idxOf? key) pool.cursor with
  | none => exact hlen
  | some index =>
    dsimp only
    cases ha : accounts[index]? with
    | none => exact hlen
    | some key =>
      exact applySessionMapAction_preserves_cap pool.sessions session key _ pool.sessionCap hlen

/-- `PoolState.choose` never modifies account limit observations: clearing or
updating the session map loses only stickiness. -/
theorem poolChoose_preserves_limits (pool : PoolState) (accounts : List String)
    (session : String) (family : Family) (now : Time) (policy : Policy) :
    (pool.choose accounts session family now policy).1.limits = pool.limits := by
  unfold PoolState.choose
  dsimp only
  cases select policy now family (accounts.map pool.snapshot)
      ((mapGet pool.sessions session).bind fun key => accounts.idxOf? key) pool.cursor with
  | none => rfl
  | some index =>
    dsimp only
    cases accounts[index]? <;> rfl

/-- When `PoolState.choose` returns a choice, that account is eligible in the
pool's current limit state. -/
theorem poolChoose_eligible {pool : PoolState} {accounts : List String}
    {session : String} {family : Family} {now : Time} {policy : Policy}
    {pool' : PoolState} {choice : PoolChoice}
    (h : pool.choose accounts session family now policy = (pool', some choice)) :
    accounts[choice.index]? = some choice.key ∧
      ∃ tier, tier? policy now family (pool.snapshot choice.key) = some tier := by
  unfold PoolState.choose at h
  dsimp only at h
  cases hs : select policy now family (accounts.map pool.snapshot)
      ((mapGet pool.sessions session).bind fun key => accounts.idxOf? key) pool.cursor with
  | none => simp [hs] at h
  | some index =>
    rw [hs] at h
    dsimp only at h
    cases ha : accounts[index]? with
    | none => simp [ha] at h
    | some key =>
      rw [ha] at h
      dsimp only at h
      cases h
      obtain ⟨acc, tier, hacc, htier⟩ := select_eligible hs
      rw [List.getElem?_map, ha] at hacc
      cases hacc
      exact ⟨ha, tier, htier⟩

/-- A session whose recorded account stays in the best available tier keeps its
binding, does not mark `switched`, and leaves both `cursor` and `sessions`
untouched. -/
theorem poolChoose_sticky_preserves_healthy {pool : PoolState} {accounts : List String}
    {session key : String} {family : Family} {now : Time} {policy : Policy}
    {c : Nat} {tier : Tier}
    (hrec : mapGet pool.sessions session = some key)
    (hidx : accounts.idxOf? key = some c)
    (hacc : accounts[c]? = some key)
    (htier : tier? policy now family (pool.snapshot key) = some tier)
    (hbest : ∀ (j : Nat) (other : Account) (otherTier : Tier),
      (accounts.map pool.snapshot)[j]? = some other →
      tier? policy now family other = some otherTier → tier.rank ≤ otherTier.rank) :
    pool.choose accounts session family now policy =
      (pool, some { key, index := c, switched := false }) := by
  have hmap : (accounts.map pool.snapshot)[c]? = some (pool.snapshot key) := by
    rw [List.getElem?_map, hacc]
    rfl
  have hsel : select policy now family (accounts.map pool.snapshot) (some c) pool.cursor = some c :=
    select_sticky hmap htier hbest
  unfold PoolState.choose
  simp [hrec, hidx, hsel, hacc, nextCursor, cursorAction, sessionMapAction, applySessionMapAction]

/-- `PoolState.choose` returns `none` when every listed account is exhausted,
leaving the pool state unchanged. -/
theorem poolChoose_none_exhausted {pool : PoolState} {accounts : List String}
    {session : String} {family : Family} {now : Time} {policy : Policy}
    {pool' : PoolState}
    (h : pool.choose accounts session family now policy = (pool', none)) :
    pool' = pool ∧
      ∀ (i : Nat) (key : String), accounts[i]? = some key →
        tier? policy now family (pool.snapshot key) = none := by
  unfold PoolState.choose at h
  dsimp only at h
  cases hs : select policy now family (accounts.map pool.snapshot)
      ((mapGet pool.sessions session).bind fun key => accounts.idxOf? key) pool.cursor with
  | none =>
    simp only [hs, Prod.mk.injEq] at h
    refine ⟨h.1.symm, ?_⟩
    intro i key ha
    have hmap : (accounts.map pool.snapshot)[i]? = some (pool.snapshot key) := by
      rw [List.getElem?_map, ha]
      rfl
    exact select_none_exhausted hs i (pool.snapshot key) hmap
  | some index =>
    simp only [hs] at h
    cases ha : accounts[index]? with
    | none =>
      obtain ⟨acc, _, hacc, _⟩ := select_eligible hs
      rw [List.getElem?_map, ha] at hacc
      simp at hacc
    | some key => simp [ha] at h

theorem le_maxBlockingReset (now : Time) (family : Family) :
    ∀ (limits : List Limit) (acc : Time), acc ≤ maxBlockingReset now family limits acc
  | [], acc => Nat.le_refl acc
  | l :: rest, acc => by
    unfold maxBlockingReset
    simp only [List.foldl_cons]
    have hstep : acc ≤ if isBlockingLimit now family l then max acc l.resetAt else acc := by
      split
      · exact Nat.le_max_left _ _
      · exact Nat.le_refl _
    exact Nat.le_trans hstep (le_maxBlockingReset now family rest _)

theorem maxBlockingReset_ge_mem (now : Time) (family : Family) :
    ∀ (limits : List Limit) (acc : Time) {l : Limit},
      l ∈ limits → isBlockingLimit now family l = true →
      l.resetAt ≤ maxBlockingReset now family limits acc
  | [], _, _, hmem, _ => by cases hmem
  | hd :: rest, acc, l, hmem, hblk => by
    unfold maxBlockingReset
    simp only [List.foldl_cons]
    rcases List.mem_cons.mp hmem with rfl | hrest
    · simp only [hblk, ↓reduceIte]
      exact Nat.le_trans (Nat.le_max_right acc l.resetAt)
        (le_maxBlockingReset now family rest (max acc l.resetAt))
    · exact maxBlockingReset_ge_mem now family rest _ hrest hblk

theorem maxBlockingReset_witness (now : Time) (family : Family) :
    ∀ (limits : List Limit) (acc : Time),
      maxBlockingReset now family limits acc = acc ∨
        ∃ l ∈ limits, isBlockingLimit now family l = true ∧
          maxBlockingReset now family limits acc = l.resetAt
  | [], acc => Or.inl rfl
  | hd :: rest, acc => by
    have hstep : maxBlockingReset now family (hd :: rest) acc =
        maxBlockingReset now family rest
          (if isBlockingLimit now family hd then max acc hd.resetAt else acc) := rfl
    rw [hstep]
    rcases maxBlockingReset_witness now family rest
        (if isBlockingLimit now family hd then max acc hd.resetAt else acc) with
      hacc | ⟨l, hmem, hblk, heq⟩
    · by_cases hhd : isBlockingLimit now family hd = true
      · simp only [hhd, ↓reduceIte] at hacc ⊢
        by_cases hle : acc ≤ hd.resetAt
        · rw [Nat.max_eq_right hle] at hacc ⊢
          exact Or.inr ⟨hd, List.mem_cons_self _ _, hhd, hacc⟩
        · have hge : hd.resetAt ≤ acc := Nat.le_of_not_ge hle
          rw [Nat.max_eq_left hge] at hacc ⊢
          exact Or.inl hacc
      · simp only [hhd, Bool.false_eq_true, ↓reduceIte] at hacc ⊢
        exact Or.inl hacc
    · exact Or.inr ⟨l, List.mem_cons_of_mem hd hmem, hblk, heq⟩

theorem accountResetAt_ge_now (policy : Policy) (now : Time) (family : Family) (account : Account) :
    now ≤ accountResetAt policy now family account := by
  unfold accountResetAt
  split
  · exact Nat.le_max_left _ _
  · exact Nat.le_trans (Nat.le_max_left _ _) (le_maxBlockingReset now family account.limits _)

theorem accountResetAt_ge_blocked (policy : Policy) (now : Time) (family : Family)
    (account : Account) :
    account.blockedUntil ≤ accountResetAt policy now family account := by
  unfold accountResetAt
  split
  · exact Nat.le_max_right _ _
  · exact Nat.le_trans (Nat.le_max_right _ _) (le_maxBlockingReset now family account.limits _)

/-- An enabled account is guaranteed eligible at `accountResetAt`. -/
theorem accountResetAt_eligible (policy : Policy) (now : Time) (family : Family)
    (account : Account) :
    ∃ t, tier? policy (accountResetAt policy now family account) family
      { account with enabled := true } = some t := by
  have hnotblk : ¬(accountResetAt policy now family account < account.blockedUntil) :=
    Nat.not_lt.mpr (accountResetAt_ge_blocked policy now family account)
  have hnow : now ≤ accountResetAt policy now family account :=
    accountResetAt_ge_now policy now family account
  unfold tier? tierOfStatus
  simp only [Bool.not_true, decide_eq_true_eq, hnotblk, Bool.or_false, ↓reduceIte]
  by_cases hover : (policy.allowOverage && account.overage.serves) = true
  · cases liveLimitStatus (accountResetAt policy now family account) family
        { account with enabled := true } with
    | clear => exact ⟨.clear, rfl⟩
    | warned => exact ⟨.warned, rfl⟩
    | rejected => exact ⟨.overage, by simp [hover]⟩
  · have hnorej : (liveLimits (accountResetAt policy now family account) family
        { account with enabled := true }).any (·.severity == .rejected) = false := by
      rw [Bool.eq_false_iff]
      intro hany
      rw [List.any_eq_true] at hany
      obtain ⟨l, hlmem, hlrej⟩ := hany
      simp only [liveLimits, List.mem_filter, Limit.live, Bool.and_eq_true, decide_eq_true_eq] at hlmem
      rcases hlmem with ⟨hin, hrlt, hbinds⟩
      have hnowlt : now < l.resetAt := Nat.lt_of_le_of_lt hnow hrlt
      have hblk : isBlockingLimit now family l = true := by
        simp [isBlockingLimit, hlrej, hnowlt, hbinds]
      have hle : l.resetAt ≤ accountResetAt policy now family account := by
        have hoverf : (policy.allowOverage && account.overage.serves) = false :=
          Bool.eq_false_iff.mpr hover
        unfold accountResetAt
        simp only [hoverf, Bool.false_eq_true, ↓reduceIte]
        exact maxBlockingReset_ge_mem now family account.limits _ hin hblk
      exact Nat.not_lt_of_ge hle hrlt
    cases hst : liveLimitStatus (accountResetAt policy now family account) family
        { account with enabled := true } with
    | clear => exact ⟨.clear, rfl⟩
    | warned => exact ⟨.warned, rfl⟩
    | rejected =>
      unfold liveLimitStatus at hst
      simp [hnorej] at hst
      split at hst <;> cases hst

/-- Strictly before `accountResetAt` (at or after `now`), an enabled account
cannot serve `family` under `policy`. -/
theorem accountResetAt_minimal (policy : Policy) (now t : Time) (family : Family)
    (account : Account) (hnow : now ≤ t)
    (hlt : t < accountResetAt policy now family account) :
    tier? policy t family { account with enabled := true } = none := by
  unfold accountResetAt at hlt
  by_cases hover : (policy.allowOverage && account.overage.serves) = true
  · simp only [hover, ↓reduceIte] at hlt
    have hblk : t < account.blockedUntil := by
      rcases Nat.le_total now account.blockedUntil with h | h
      · rwa [Nat.max_eq_right h] at hlt
      · rw [Nat.max_eq_left h] at hlt
        exact absurd hlt (Nat.not_lt_of_ge hnow)
    simp [tier?, tierOfStatus, hblk]
  · have hoverf : (policy.allowOverage && account.overage.serves) = false :=
      Bool.eq_false_iff.mpr hover
    simp only [hoverf, Bool.false_eq_true, ↓reduceIte] at hlt
    rcases maxBlockingReset_witness now family account.limits (max now account.blockedUntil) with
      hbase | ⟨l, hmem, hblk, heq⟩
    · rw [hbase] at hlt
      have htb : t < account.blockedUntil := by
        rcases Nat.le_total now account.blockedUntil with h | h
        · rwa [Nat.max_eq_right h] at hlt
        · rw [Nat.max_eq_left h] at hlt
          exact absurd hlt (Nat.not_lt_of_ge hnow)
      simp [tier?, tierOfStatus, htb]
    · rw [heq] at hlt
      simp only [isBlockingLimit, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at hblk
      rcases hblk with ⟨⟨hsev, _⟩, hbinds⟩
      have hlive : l ∈ liveLimits t family { account with enabled := true } := by
        simp [liveLimits, Limit.live, hmem, hlt, hbinds]
      have hany : (liveLimits t family { account with enabled := true }).any
          (·.severity == .rejected) = true :=
        List.any_eq_true.mpr ⟨l, hlive, by simp [hsev]⟩
      have hst : liveLimitStatus t family { account with enabled := true } = .rejected := by
        simp [liveLimitStatus, hany]
      unfold tier? tierOfStatus
      split
      · rfl
      · simp [hst, hoverf]

theorem earliestReset_none_iff {policy : Policy} {now : Time} {family : Family}
    {accounts : List Account} :
    earliestReset policy now family accounts = none ↔ accounts = [] := by
  cases accounts with
  | nil => simp [earliestReset]
  | cons hd rest =>
    simp only [earliestReset, reduceCtorEq, iff_false]
    cases earliestReset policy now family rest <;> simp

theorem earliestReset_mem {policy : Policy} {now : Time} {family : Family} :
    ∀ {accounts : List Account} {r : Time},
      earliestReset policy now family accounts = some r →
      ∃ a ∈ accounts, r = accountResetAt policy now family { a with enabled := true }
  | [], _, h => by simp [earliestReset] at h
  | hd :: rest, r, h => by
    simp only [earliestReset] at h
    cases hrest : earliestReset policy now family rest with
    | none =>
      simp only [hrest, Option.some.injEq] at h
      exact ⟨hd, List.mem_cons_self _ _, h.symm⟩
    | some best =>
      simp only [hrest, Option.some.injEq] at h
      by_cases hle : accountResetAt policy now family { hd with enabled := true } ≤ best
      · rw [Nat.min_eq_left hle] at h
        exact ⟨hd, List.mem_cons_self _ _, h.symm⟩
      · have hge : best ≤ accountResetAt policy now family { hd with enabled := true } :=
          Nat.le_of_not_ge hle
        rw [Nat.min_eq_right hge] at h
        obtain ⟨a, hmem, heq⟩ := earliestReset_mem hrest
        exact ⟨a, List.mem_cons_of_mem hd hmem, h.symm.trans heq⟩

theorem earliestReset_le {policy : Policy} {now : Time} {family : Family} :
    ∀ {accounts : List Account} {r : Time},
      earliestReset policy now family accounts = some r →
      ∀ a ∈ accounts, r ≤ accountResetAt policy now family { a with enabled := true }
  | [], _, h, _, hmem => by cases hmem
  | hd :: rest, r, h, a, hmem => by
    simp only [earliestReset] at h
    cases hrest : earliestReset policy now family rest with
    | none =>
      have hnil : rest = [] := earliestReset_none_iff.mp hrest
      subst hnil
      simp only [hrest, Option.some.injEq] at h
      rcases List.mem_cons.mp hmem with rfl | hcontra
      · exact Nat.le_of_eq h.symm
      · cases hcontra
    | some best =>
      simp only [hrest, Option.some.injEq] at h
      rcases List.mem_cons.mp hmem with rfl | hin
      · rw [← h]
        exact Nat.min_le_left _ _
      · rw [← h]
        exact Nat.le_trans (Nat.min_le_right _ _) (earliestReset_le hrest a hin)

/-- At `earliestReset`, some listed account is eligible to serve. -/
theorem earliestReset_eligible {policy : Policy} {now : Time} {family : Family}
    {accounts : List Account} {r : Time}
    (h : earliestReset policy now family accounts = some r) :
    now ≤ r ∧
      ∃ a ∈ accounts, ∃ t, tier? policy r family { a with enabled := true } = some t := by
  obtain ⟨a, hmem, hr⟩ := earliestReset_mem h
  subst hr
  refine ⟨accountResetAt_ge_now policy now family { a with enabled := true }, a, hmem, ?_⟩
  simpa using accountResetAt_eligible policy now family { a with enabled := true }

/-- Strictly before `earliestReset` (at or after `now`), no listed account can
serve `family` under `policy`. -/
theorem earliestReset_minimal {policy : Policy} {now t : Time} {family : Family}
    {accounts : List Account} {r : Time}
    (h : earliestReset policy now family accounts = some r)
    (hnow : now ≤ t) (hlt : t < r) :
    ∀ a ∈ accounts, tier? policy t family { a with enabled := true } = none := by
  intro a hmem
  have hle := earliestReset_le h a hmem
  simpa using accountResetAt_minimal policy now t family { a with enabled := true } hnow
    (Nat.lt_of_lt_of_le hlt hle)

end AccountRotation
