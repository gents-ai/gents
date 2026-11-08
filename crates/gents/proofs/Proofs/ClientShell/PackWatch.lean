/-!
# Pack UX data watch

A pack page's live observation of one datastore tool: the desktop bridge's
task behind `desktop_ux_data_watch` (`gents-desktop-bridge::ux_watch::run`)
reading the runtime's `POST /packs/ux/watch` server-sent events
(`gents-cli::http::pack_watch::watch`).
-/

namespace ClientShell.PackWatch

/-- Why a watch stopped for good. Both reasons name the same next action: the
page must be reopened, which starts a new watch. -/
inductive TerminalReason where
  /-- The runtime refused the User's freshly acquired credentials. -/
  | auth
  /-- The request or the stream is malformed: a rejected query, an
  undecodable or oversized event, or an endpoint that is not a managed
  runtime. Retrying would repeat it. -/
  | permanent
  deriving DecidableEq, Repr

/-- Consecutive failures stop counting here: the delay is already capped. -/
def maxFailures : Nat := 5

/-- The watch task's phase.

`starting` acquires the runtime endpoint and the User's signed authorization
from the client core and connects; every connect re-acquires them, so a
runtime restart (a new port) or a renewed credential is picked up by the next
attempt rather than ending the watch. `failures` counts consecutive failed
attempts since the last accepted connection. -/
inductive Phase where
  | starting (failures : Nat)
  | streaming
  | backoff (failures : Nat)
  | terminal (reason : TerminalReason)
  | cancelled
  deriving DecidableEq, Repr

/-- What the watch task observes.

`transientError` is a connect, send or read failure or timeout, a truncated
stream, a retryable status (408, 425, 429, 5xx), the runtime's own `error`
event (whose text asks the client to reconnect), or an endpoint that cannot be
acquired while the client core or its route is restarting. `authError` is a
401 or 403. `permanentError` is any other refusal, an oversized (8 MiB) or
undecodable event, or a local signing failure.

`cancel` is the page's unwatch, the window's destruction, or a closed channel.
`nodeDeselected` is the window selecting another node: the selection change
and the registry cancellation happen under the bridge lock, so the task
observes it as the same token cancellation. `retry` is the backoff timer. -/
inductive Event where
  | connected
  | cleanEnd
  | transientError
  | authError
  | permanentError
  | cancel
  | nodeDeselected
  | retry
  deriving DecidableEq, Repr

def Phase.isFinal : Phase → Bool
  | .terminal _ => true
  | .cancelled => true
  | _ => false

def nextFailures (failures : Nat) : Nat := min (failures + 1) maxFailures

/-- The minimum spacing, in milliseconds, between the start of an attempt and
the start of the next one after `failures` consecutive failures. A clean end
is `backoff 0`: the runtime bounds each stream (30 s) and re-sends a full
snapshot on every connect, so one second between connects is enough to heal
the gap. -/
def delayMs (failures : Nat) : Nat := min (1000 * 2 ^ failures) 30000

/-- External premise: the runtime authenticates the signed request before it
opens the stream, so an authorization failure arrives only as the connect
status. A stream that ends after it was accepted is treated as transient. -/
def step : Phase → Event → Phase
  | .terminal reason, _ => .terminal reason
  | .cancelled, _ => .cancelled
  | _, .cancel => .cancelled
  | _, .nodeDeselected => .cancelled
  | .starting _, .connected => .streaming
  | .starting failures, .cleanEnd => .backoff (nextFailures failures)
  | .starting failures, .transientError => .backoff (nextFailures failures)
  | .starting _, .authError => .terminal .auth
  | .starting _, .permanentError => .terminal .permanent
  | .streaming, .cleanEnd => .backoff 0
  | .streaming, .transientError => .backoff (nextFailures 0)
  | .streaming, .authError => .backoff (nextFailures 0)
  | .streaming, .permanentError => .terminal .permanent
  | .backoff failures, .retry => .starting failures
  | phase, _ => phase

/-- The next action a terminal view names. -/
inductive NextAction where
  | reopenPage
  deriving DecidableEq, Repr

/-- What the pack page shows.

Decision: the wire event keeps its two forms (snapshot, error). While
`connecting` or `reconnecting` the page keeps its last snapshot; the task is
progressing on a bounded timer, and the next accepted connection re-sends a
full snapshot. `closed` sends nothing: the page unwatched, its window is gone,
or the shell changed node, and the page itself receives `shell-changed` with
the new node and decides whether to watch it. -/
inductive View where
  | connecting
  | live
  | reconnecting (delayMs : Nat)
  | unavailable (reason : TerminalReason) (next : NextAction)
  | closed
  deriving DecidableEq, Repr

def view : Phase → View
  | .starting _ => .connecting
  | .streaming => .live
  | .backoff failures => .reconnecting (delayMs failures)
  | .terminal reason => .unavailable reason .reopenPage
  | .cancelled => .closed

def View.progressing : View → Bool
  | .connecting => true
  | .live => true
  | .reconnecting _ => true
  | _ => false

theorem transient_errors_are_not_terminal (phase : Phase)
    (h : phase.isFinal = false) :
    (step phase .transientError).isFinal = false ∧
      (step phase .cleanEnd).isFinal = false := by
  cases phase <;> simp_all [step, Phase.isFinal]

theorem node_deselected_cancels (phase : Phase) (h : phase.isFinal = false) :
    step phase .nodeDeselected = .cancelled := by
  cases phase <;> simp_all [step, Phase.isFinal]

theorem node_deselected_is_cancel (phase : Phase) :
    step phase .nodeDeselected = step phase .cancel := by
  cases phase <;> rfl

theorem cancel_cancels (phase : Phase) (h : phase.isFinal = false) :
    step phase .cancel = .cancelled := by
  cases phase <;> simp_all [step, Phase.isFinal]

theorem delay_positive (failures : Nat) : 0 < delayMs failures := by
  unfold delayMs
  exact Nat.lt_min.mpr ⟨Nat.mul_pos (by decide) (Nat.two_pow_pos _), by decide⟩

theorem delay_bounded (failures : Nat) : delayMs failures ≤ 30000 :=
  Nat.min_le_right _ _

theorem clean_end_waits_one_second : delayMs 0 = 1000 := by decide

theorem failures_stay_bounded (phase : Phase) (event : Event)
    (h : ∀ f, phase = .starting f ∨ phase = .backoff f → f ≤ maxFailures) :
    ∀ f, step phase event = .starting f ∨ step phase event = .backoff f →
      f ≤ maxFailures := by
  intro f hf
  have bound : ∀ g, nextFailures g ≤ maxFailures := fun g => Nat.min_le_right _ _
  cases phase <;> cases event <;> simp_all [step, maxFailures] <;>
    first
    | (rcases hf with hf | hf <;> subst hf <;> exact bound _)
    | (subst hf; exact bound _)
    | (subst hf; simp)
    | omega

/-- Only a permanent error, or an authorization refusal of credentials that
the attempt acquired freshly (every `starting` attempt re-acquires them),
ends the watch with an error. -/
theorem terminal_only_from_permanent_or_reacquired_auth
    (phase : Phase) (event : Event) (reason : TerminalReason)
    (h : phase.isFinal = false) (hstep : step phase event = .terminal reason) :
    (event = .permanentError ∧ reason = .permanent) ∨
      (event = .authError ∧ reason = .auth ∧ ∃ failures, phase = .starting failures) := by
  cases phase <;> cases event <;> simp_all [step, Phase.isFinal] <;>
    first | (subst hstep; simp) | exact hstep.symm

theorem backoff_retry_reacquires (failures : Nat) :
    step (.backoff failures) .retry = .starting failures := rfl

theorem accepted_connection_resets_failures (failures : Nat) :
    step (.starting failures) .connected = .streaming ∧
      step .streaming .transientError = .backoff 1 :=
  ⟨rfl, by decide⟩

/-- Nothing gets silently stuck: every view is progressing on its own, or names
its cause and the next action, or is closed because nobody watches it. -/
theorem every_view_progresses_or_names_its_cause (phase : Phase) :
    (view phase).progressing = true ∨
      (∃ reason, view phase = .unavailable reason .reopenPage) ∨
      view phase = .closed := by
  cases phase <;> simp [view, View.progressing]

/-- Progressing views leave on their own: a backoff retries, and a connecting
or live view always has a network outcome that moves it. -/
theorem progressing_phases_have_an_unattended_exit (phase : Phase)
    (h : (view phase).progressing = true) :
    ∃ event, event ≠ .cancel ∧ event ≠ .nodeDeselected ∧ step phase event ≠ phase := by
  cases phase with
  | starting failures => exact ⟨.connected, by decide, by decide, by simp [step]⟩
  | streaming => exact ⟨.cleanEnd, by decide, by decide, by simp [step]⟩
  | backoff failures => exact ⟨.retry, by decide, by decide, by simp [step]⟩
  | terminal reason => simp [view, View.progressing] at h
  | cancelled => simp [view, View.progressing] at h

end ClientShell.PackWatch
