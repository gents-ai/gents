import Proofs.AccountRotation.Properties

/-!
Executable rotation witnesses. The single-account cases replay Claude Code
2.1.311's rate-limit simulator scenarios (`normal`, `session-limit-reached`,
`approaching-weekly-limit`, `weekly-limit-reached`, `overage-active`,
`overage-warning`, `overage-exhausted`, `opus-limit`, `sonnet-warning`) at
`now = 1000`, with the simulator's five-hour and seven-day resets.
-/

namespace AccountRotation

def caseNow : Time := 1000
def fiveHourReset : Time := caseNow + 18000
def sevenDayReset : Time := caseNow + 604800

def clearAccount : Account :=
  { enabled := true, limits := [], overage := .absent, blockedUntil := 0 }

def limited (claim : Claim) (severity : Severity) (resetAt : Time)
    (overage : Overage := .absent) : Account :=
  { clearAccount with limits := [⟨claim, severity, resetAt⟩], overage }

def warnedAccount : Account := limited .sevenDay .warning sevenDayReset
def rejectedAccount : Account := limited .fiveHour .rejected fiveHourReset

structure SelectCase where
  name : String
  policy : Policy
  family : Family
  pool : List Account
  current : Option Nat
  cursor : Nat
  expected : Option Nat
  deriving Repr

def selectCase (name : String) (pool : List Account) (current : Option Nat := none)
    (cursor : Nat := 0) (family : Family := .opus) (policy : Policy := {}) : SelectCase :=
  { name, policy, family, pool, current, cursor,
    expected := select policy caseNow family pool current cursor }

def selectCases : List SelectCase :=
  [ selectCase "normal" [clearAccount]
  , selectCase "session_limit_reached" [rejectedAccount]
  , selectCase "approaching_weekly_limit" [warnedAccount]
  , selectCase "weekly_limit_reached" [limited .sevenDay .rejected sevenDayReset]
  , selectCase "overage_active_policy_off"
      [limited .fiveHour .rejected fiveHourReset .allowed]
  , selectCase "overage_active_policy_on"
      [limited .fiveHour .rejected fiveHourReset .allowed] (policy := { allowOverage := true })
  , selectCase "overage_warning_policy_on"
      [limited .fiveHour .rejected fiveHourReset .warning] (policy := { allowOverage := true })
  , selectCase "overage_exhausted_policy_on"
      [limited .fiveHour .rejected fiveHourReset .rejected] (policy := { allowOverage := true })
  , selectCase "opus_limit_blocks_opus" [limited .sevenDayOpus .rejected sevenDayReset]
  , selectCase "opus_limit_spares_sonnet" [limited .sevenDayOpus .rejected sevenDayReset]
      (family := .sonnet)
  , selectCase "sonnet_warning_spares_opus" [limited .sevenDaySonnet .warning sevenDayReset]
  , selectCase "limit_after_reset_is_clear" [limited .fiveHour .rejected 900]
  , selectCase "disabled_account_skipped" [{ clearAccount with enabled := false }, clearAccount]
  , selectCase "blocked_account_skipped" [{ clearAccount with blockedUntil := 2000 }, clearAccount]
  , selectCase "session_stays_while_clear" [clearAccount, clearAccount, clearAccount] (some 1)
  , selectCase "warning_moves_to_next_clear" [clearAccount, warnedAccount, clearAccount] (some 1)
  , selectCase "warning_wraps_to_first_clear" [clearAccount, clearAccount, warnedAccount] (some 2)
  , selectCase "all_warned_stays" [warnedAccount, warnedAccount, warnedAccount] (some 1)
  , selectCase "warned_session_moves_to_clear" [warnedAccount, warnedAccount, clearAccount]
      (some 0)
  , selectCase "new_session_takes_cursor" [clearAccount, clearAccount, clearAccount] none 2
  , selectCase "new_session_cursor_skips_limited" [clearAccount, rejectedAccount, clearAccount]
      none 1
  , selectCase "cursor_past_end_wraps" [clearAccount, clearAccount] none 5
  , selectCase "all_exhausted_blocks"
      [rejectedAccount, limited .fiveHour .rejected fiveHourReset .allowed]
  , selectCase "overage_last_resort_policy_on"
      [rejectedAccount, limited .fiveHour .rejected fiveHourReset .allowed] (some 0)
      (policy := { allowOverage := true })
  , selectCase "warned_preferred_over_overage"
      [limited .fiveHour .rejected fiveHourReset .allowed, warnedAccount] (some 0)
      (policy := { allowOverage := true })
  , selectCase "family_limit_rotates_opus"
      [limited .sevenDayOpus .rejected sevenDayReset, clearAccount] (some 0)
  , selectCase "family_limit_keeps_sonnet"
      [limited .sevenDayOpus .rejected sevenDayReset, clearAccount] (some 0) (family := .sonnet)
  ]

theorem selectCases_expected :
    selectCases.map (·.expected) =
      [some 0, none, some 0, none, none, some 0, some 0, none, none, some 0, some 0, some 0,
       some 1, some 1, some 1, some 2, some 0, some 1, some 2, some 2, some 2, some 0, none,
       some 1, some 1, some 1, some 0] := by
  native_decide

structure AttemptCase where
  name : String
  policy : Policy
  family : Family
  pool : List Account
  current : Option Nat
  cursor : Nat
  /-- Accounts the provider refuses during the attempt, with the reported reset. -/
  rejects : List (Nat × Time)
  expected : List Nat
  deriving Repr

def rejectsOf (refusals : List (Nat × Time)) (i : Nat) : Option Time :=
  (refusals.find? (·.1 == i)).map (·.2)

def attemptCase (name : String) (pool : List Account) (rejects : List (Nat × Time))
    (current : Option Nat := some 0) (family : Family := .opus) (policy : Policy := {}) :
    AttemptCase :=
  { name, policy, family, pool, current, cursor := 0, rejects,
    expected := attempt policy caseNow family (rejectsOf rejects) pool.length pool current 0 }

def attemptCases : List AttemptCase :=
  [ attemptCase "served_first" [clearAccount, clearAccount, clearAccount] []
  , attemptCase "rejection_replays_on_next" [clearAccount, clearAccount, clearAccount]
      [(0, fiveHourReset)]
  , attemptCase "rejections_walk_pool_once" [clearAccount, clearAccount, clearAccount]
      [(0, fiveHourReset), (1, fiveHourReset), (2, fiveHourReset)]
  , attemptCase "rejection_without_reset_still_moves_on" [clearAccount, clearAccount]
      [(0, 0)]
  , attemptCase "rejection_skips_known_limited" [clearAccount, rejectedAccount, clearAccount]
      [(0, fiveHourReset)]
  , attemptCase "exhausted_pool_sends_nothing" [rejectedAccount, rejectedAccount] []
  ]

theorem attemptCases_expected :
    attemptCases.map (·.expected) = [[0], [0, 1], [0, 1, 2], [0, 1], [0, 2], []] := by
  native_decide

structure ObserveCase where
  name : String
  family : Family
  account : Account
  reported : List Limit
  overage : Overage
  expected : Account
  deriving Repr

def observeCase (name : String) (family : Family) (account : Account) (reported : List Limit)
    (overage : Overage := .absent) : ObserveCase :=
  { name, family, account, reported, overage,
    expected := observe family reported overage account }

def observeCases : List ObserveCase :=
  [ observeCase "allowed_clears_family_limits" .opus warnedAccount []
  , observeCase "warning_is_recorded" .opus clearAccount
      [⟨.sevenDay, .warning, sevenDayReset⟩]
  , observeCase "report_keeps_other_family_limits" .sonnet
      { clearAccount with
        limits := [⟨.fiveHour, .warning, fiveHourReset⟩,
          ⟨.sevenDayOpus, .rejected, sevenDayReset⟩] } []
  , observeCase "overage_status_is_replaced" .opus
      (limited .fiveHour .rejected fiveHourReset .allowed)
      [⟨.fiveHour, .rejected, fiveHourReset⟩] .rejected
  ]

theorem observeCases_expected :
    observeCases.map (·.expected.limits) =
      [[], [⟨.sevenDay, .warning, sevenDayReset⟩], [⟨.sevenDayOpus, .rejected, sevenDayReset⟩],
       [⟨.fiveHour, .rejected, fiveHourReset⟩]] := by
  native_decide

end AccountRotation
