/-!
# Pack UX data watch

A pack page's live observation of one datastore tool: the desktop bridge's
task behind `desktop_ux_data_watch` (`gents-desktop-bridge::ux_watch::run`)
reading the runtime's `POST /packs/ux/watch` server-sent events
(`gents-cli::http::pack_watch::watch`).
-/

namespace ClientShell.PackWatch

/-- Why a watch stopped for good. Every reason names the same next action:
the page must be reopened, which starts a new watch. -/
inductive TerminalReason where
  /-- The runtime refused the User's freshly acquired credentials. -/
  | auth
  /-- The request or the stream is malformed, or the runtime cannot be
  reached through this desktop at all (its record names another home, no
  local home, or no operator endpoint, or the client key cannot sign).
  Retrying would repeat it. -/
  | permanent
  /-- `retryBudget` consecutive attempts failed without one stream ending
  cleanly: the runtime is stopped, refuses connections, keeps answering a
  retryable status, or accepts and then fails every stream. -/
  | unreachable
  deriving DecidableEq, Repr

/-- Consecutive failed attempts that end the watch.

Decision: the delays before the 2nd through 8th attempts are 2, 4, 8, 16, 30,
30 and 30 seconds, so a watch gives up about two minutes after its runtime
stops answering. A runtime restart (a new port and credential) takes seconds
and is healed within the budget; an outage longer than that is reported, not
hidden behind an indicator that never resolves. -/
def retryBudget : Nat := 8

/-- The watch task's phase.

`starting` acquires the runtime endpoint and the User's signed authorization
from the client core and connects; every connect re-acquires them, so a
runtime restart (a new port) or a renewed credential is picked up by the next
attempt rather than ending the watch. `failures` counts consecutive failed
attempts since the last stream that ended cleanly; an accepted connection
keeps the count, because a runtime that accepts and then fails every stream
is as unavailable as one that refuses. -/
inductive Phase where
  | starting (failures : Nat)
  | streaming (failures : Nat)
  | backoff (failures : Nat)
  | terminal (reason : TerminalReason)
  | cancelled
  deriving DecidableEq, Repr

/-- What the watch task observes.

`transientError` is a connect, send or read failure or timeout, a truncated
stream, a retryable status (408, 425, 429, 5xx), the runtime's own `error`
event (whose text asks the client to reconnect), or a client core that is
restarting. `authError` is a 401 or 403. `permanentError` is any other
refusal, an oversized (8 MiB) or undecodable event, a runtime record this
desktop cannot reach (`LocalHomeAccessError`), or a local signing failure.

`cancel` is the page's unwatch, the window's destruction, a closed channel,
or the window selecting another node. The task cannot tell these apart and
does not need to; the ordering that keeps a selection change from delivering
the old node's data is TLA `PackUxWatch` `CancelBeforeSelect`. `retry` is the
backoff timer. -/
inductive Event where
  | connected
  | cleanEnd
  | transientError
  | authError
  | permanentError
  | cancel
  | retry
  deriving DecidableEq, Repr

def Phase.isFinal : Phase → Bool
  | .terminal _ => true
  | .cancelled => true
  | _ => false

/-- The minimum spacing, in milliseconds, between the start of an attempt and
the start of the next one after `failures` consecutive failures. A clean end
is `backoff 0`: the runtime bounds each stream (30 s) and re-sends a full
snapshot on every connect, so one second between connects is enough to heal
the gap. -/
def delayMs (failures : Nat) : Nat := min (1000 * 2 ^ failures) 30000

/-- One more failed attempt: back off, or end the watch once the budget is
spent. -/
def fail (failures : Nat) : Phase :=
  if failures + 1 < retryBudget then .backoff (failures + 1) else .terminal .unreachable

/-- External premise: the runtime authenticates the signed request before it
opens the stream, so an authorization failure arrives only as the connect
status. A stream that ends with one after it was accepted is a failure like
any other. -/
def step : Phase → Event → Phase
  | .terminal reason, _ => .terminal reason
  | .cancelled, _ => .cancelled
  | _, .cancel => .cancelled
  | .starting failures, .connected => .streaming failures
  | .starting failures, .cleanEnd => fail failures
  | .starting failures, .transientError => fail failures
  | .starting _, .authError => .terminal .auth
  | .starting _, .permanentError => .terminal .permanent
  | .streaming _, .cleanEnd => .backoff 0
  | .streaming failures, .transientError => fail failures
  | .streaming failures, .authError => fail failures
  | .streaming _, .permanentError => .terminal .permanent
  | .backoff failures, .retry => .starting failures
  | phase, _ => phase

/-- The next action a terminal view names. -/
inductive NextAction where
  | reopenPage
  deriving DecidableEq, Repr

/-- What the pack page shows. A snapshot makes the page live; `closed` sends
nothing: the page unwatched, its window is gone, or the shell changed node,
and the page itself receives `shell-changed` with the new node and decides
whether to watch it. -/
inductive View where
  | connecting
  | live
  | reconnecting (delayMs : Nat)
  | unavailable (reason : TerminalReason) (next : NextAction)
  | closed
  deriving DecidableEq, Repr

def view : Phase → View
  | .starting _ => .connecting
  | .streaming _ => .live
  | .backoff failures => .reconnecting (delayMs failures)
  | .terminal reason => .unavailable reason .reopenPage
  | .cancelled => .closed

/-- What the task sends the page on entering a phase, besides the snapshots a
stream carries.

Decision: the page learns of a reconnect when it starts, not only when it
fails for good, so a stalled stream is never shown as live. The next
snapshot is the page's signal that it is live again; `starting` sends
nothing, since the page still shows the reconnect it was told of. -/
inductive Notice where
  | reconnecting (delayMs : Nat)
  | unavailable (reason : TerminalReason) (next : NextAction)
  deriving DecidableEq, Repr

def notice : Phase → Option Notice
  | .backoff failures => some (.reconnecting (delayMs failures))
  | .terminal reason => some (.unavailable reason .reopenPage)
  | _ => none

def View.progressing : View → Bool
  | .connecting => true
  | .live => true
  | .reconnecting _ => true
  | _ => false

/-- Failure counts stay inside the budget. -/
def Phase.wf : Phase → Prop
  | .starting failures => failures < retryBudget
  | .streaming failures => failures < retryBudget
  | .backoff failures => failures < retryBudget
  | _ => True

/-- Reconnects a phase can still make without a clean end. -/
def budgetLeft : Phase → Nat
  | .starting failures => retryBudget - 1 - failures
  | .streaming failures => retryBudget - 1 - failures
  | .backoff failures => retryBudget - failures
  | _ => 0

def isReconnect : Phase → Event → Bool
  | .backoff _, .retry => true
  | _, _ => false

/-- Reconnects a run of events makes. -/
def reconnects : Phase → List Event → Nat
  | _, [] => 0
  | phase, event :: rest =>
      (if isReconnect phase event then 1 else 0) + reconnects (step phase event) rest

theorem fail_wf (failures : Nat) : (fail failures).wf := by
  unfold fail
  split <;> simp_all [Phase.wf]

theorem fail_budget (failures : Nat) :
    budgetLeft (fail failures) ≤ retryBudget - 1 - failures := by
  unfold fail
  split <;> simp [budgetLeft] <;> omega

theorem step_preserves_wf (phase : Phase) (event : Event) (h : phase.wf) :
    (step phase event).wf := by
  cases phase <;> cases event <;> simp only [step] <;>
    first | exact fail_wf _ | exact h | simp [Phase.wf, retryBudget] | trivial

theorem step_spends_budget (phase : Phase) (event : Event) (h : phase.wf)
    (hclean : event ≠ .cleanEnd) :
    (if isReconnect phase event then 1 else 0) + budgetLeft (step phase event) ≤
      budgetLeft phase := by
  cases phase <;> cases event <;>
    simp only [step, isReconnect, Phase.wf, Bool.false_eq_true, ↓reduceIte, Nat.zero_add]
      at h hclean ⊢ <;>
    first
    | exact absurd rfl hclean
    | exact fail_budget _
    | (simp only [budgetLeft]; omega)
    | simp [budgetLeft]

/-- No hot loop: without a clean end a watch reconnects at most
`retryBudget - 1` times, however often the runtime accepts and then fails. -/
theorem reconnects_bounded_without_a_clean_end (phase : Phase) (events : List Event)
    (h : phase.wf) (hclean : Event.cleanEnd ∉ events) :
    reconnects phase events ≤ budgetLeft phase := by
  induction events generalizing phase with
  | nil => simp [reconnects]
  | cons event rest ih =>
      simp only [List.mem_cons, not_or] at hclean
      have spent := step_spends_budget phase event h (Ne.symm hclean.1)
      have rest_bound := ih (step phase event) (step_preserves_wf phase event h) hclean.2
      simp only [reconnects]
      omega

theorem a_new_watch_reconnects_fewer_than_the_budget (events : List Event)
    (hclean : Event.cleanEnd ∉ events) :
    reconnects (.starting 0) events ≤ retryBudget - 1 :=
  reconnects_bounded_without_a_clean_end _ _ (by simp [Phase.wf, retryBudget]) hclean

/-- Only a stream the runtime ended cleanly resets the failure count. -/
theorem only_a_clean_end_resets_failures (phase : Phase) (event : Event)
    (hphase : ∀ failures, phase ≠ .backoff failures)
    (hstep : step phase event = .backoff 0) :
    event = .cleanEnd := by
  cases phase <;> cases event <;> simp only [step, fail] at hstep <;>
    (try split at hstep) <;> simp_all

theorem an_accepted_stream_keeps_its_failures (failures : Nat) :
    step (.starting failures) .connected = .streaming failures ∧
      step (.streaming failures) .transientError = fail failures :=
  ⟨rfl, rfl⟩

theorem transient_errors_end_only_after_the_budget (phase : Phase) (failures : Nat)
    (h : phase = .starting failures ∨ phase = .streaming failures)
    (hleft : failures + 1 < retryBudget) :
    step phase .transientError = .backoff (failures + 1) := by
  rcases h with h | h <;> subst h <;> simp [step, fail, hleft]

theorem cancel_cancels (phase : Phase) (h : phase.isFinal = false) :
    step phase .cancel = .cancelled := by
  cases phase <;> simp_all [step, Phase.isFinal]

theorem delay_positive (failures : Nat) : 0 < delayMs failures := by
  unfold delayMs
  exact Nat.lt_min.mpr ⟨Nat.mul_pos (by decide) (Nat.two_pow_pos _), by decide⟩

theorem delay_bounded (failures : Nat) : delayMs failures ≤ 30000 :=
  Nat.min_le_right _ _

theorem clean_end_waits_one_second : delayMs 0 = 1000 := by decide

/-- The delays before the 2nd through 8th consecutive attempts. -/
theorem the_budget_spans_two_minutes :
    ((List.range (retryBudget - 1)).map fun index => delayMs (index + 1)).sum = 120000 := by
  decide

/-- Only a permanent error, an authorization refusal of credentials that the
attempt acquired freshly (every `starting` attempt re-acquires them), or a
spent budget ends the watch with an error. -/
theorem terminal_only_from_permanent_auth_or_budget
    (phase : Phase) (event : Event) (reason : TerminalReason)
    (h : phase.isFinal = false) (hstep : step phase event = .terminal reason) :
    (event = .permanentError ∧ reason = .permanent) ∨
      (event = .authError ∧ reason = .auth ∧ ∃ failures, phase = .starting failures) ∨
      (reason = .unreachable ∧ ∃ failures, retryBudget ≤ failures + 1 ∧
        (phase = .starting failures ∨ phase = .streaming failures)) := by
  cases phase <;> cases event <;> simp only [step, fail, Phase.isFinal] at hstep h <;>
    (try split at hstep) <;> (try simp_all) <;> (try omega)

theorem backoff_retry_reacquires (failures : Nat) :
    step (.backoff failures) .retry = .starting failures := rfl

/-- Nothing gets silently stuck: every transition into a phase that is not
receiving data sends the page a notice naming it, and the notice is what the
phase's view shows. -/
theorem every_stall_is_noticed (phase : Phase) :
    (∃ failures, phase = .backoff failures) ∨ (∃ reason, phase = .terminal reason) →
      ∃ shown, notice phase = some shown ∧
        (∀ delay, shown = .reconnecting delay → view phase = .reconnecting delay) ∧
        (∀ reason next, shown = .unavailable reason next →
          view phase = .unavailable reason next) := by
  rintro (⟨failures, rfl⟩ | ⟨reason, rfl⟩) <;> simp [notice, view]

/-- Every view is progressing on its own, or names its cause and the next
action, or is closed because nobody watches it. -/
theorem every_view_progresses_or_names_its_cause (phase : Phase) :
    (view phase).progressing = true ∨
      (∃ reason, view phase = .unavailable reason .reopenPage) ∨
      view phase = .closed := by
  cases phase <;> simp [view, View.progressing]

/-- Progressing views leave on their own: a backoff retries, and a connecting
or live view always has a network outcome that moves it. -/
theorem progressing_phases_have_an_unattended_exit (phase : Phase)
    (h : (view phase).progressing = true) :
    ∃ event, event ≠ .cancel ∧ step phase event ≠ phase := by
  cases phase with
  | starting failures => exact ⟨.connected, by decide, by simp [step]⟩
  | streaming failures => exact ⟨.cleanEnd, by decide, by simp [step]⟩
  | backoff failures => exact ⟨.retry, by decide, by simp [step]⟩
  | terminal reason => simp [view, View.progressing] at h
  | cancelled => simp [view, View.progressing] at h

end ClientShell.PackWatch
