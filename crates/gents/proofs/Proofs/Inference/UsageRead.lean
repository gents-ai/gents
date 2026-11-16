import Proofs.Inference.InferenceCall.LimitObservation

/-! A node-wide usage read: which of the node's accounts and backends it
contacts, what each contact reports and what it leaves stored.

How a provider answers (the billing, usage and key endpoints and their bodies)
is that provider's wire format; the model takes the class of the answer as
given. Which bodies are malformed is part of that format: a Claude usage object
with no known window is a read that reports none, where the same body from Grok
or OpenRouter is malformed. -/

namespace UsageRead

open InferenceCall.LimitObservation

inductive Provider where
  | grok
  | claude
  deriving DecidableEq, Repr

structure Account where
  provider : Provider
  slot : Option String
  enabled : Bool
  deriving DecidableEq, Repr

/-- `account` signs in as one of the node's accounts, `pool` rotates over all
of them, `keyed` and `unkeyed` are a metered API key that does or does not
resolve, and `unreported` has no usage source. -/
inductive BackendKind where
  | account
  | pool
  | keyed
  | unkeyed
  | unreported
  deriving DecidableEq, Repr

structure Backend where
  kind : BackendKind
  provider : Provider
  slot : Option String
  enabled : Bool
  deriving DecidableEq, Repr

inductive Response where
  | windows
  | throttled
  | malformed
  | unauthorized
  | failed
  | unreachable
  deriving DecidableEq, Repr

def Response.error : Response → Option String
  | .windows => none
  | .throttled => some "throttled"
  | .malformed => some "malformed"
  | .unauthorized => some "unauthorized"
  | .failed => some "http 500"
  | .unreachable => some "unreachable"

inductive Subject where
  | account (index : Nat)
  | backend (index : Nat)
  deriving DecidableEq, Repr

structure Job where
  subject : Subject
  live : Bool
  deriving DecidableEq, Repr

def admits (filter : Option Provider) (provider : Provider) : Bool :=
  filter.all fun wanted => wanted == provider

/-- Every account of the node is read once, whichever backends name it and
whether or not any does: a disabled backend does not hide its account, two
backends on one account do not read it twice, and a backend naming an account
the node does not hold reads nothing. A pool is read through its accounts. A
backend that signs in as no account is read as itself, and is left out when the
read asks for one provider's accounts. -/
def plan (accounts : List Account) (backends : List Backend) (filter : Option Provider) :
    List Job :=
  (accounts.zipIdx.filterMap fun (account, index) =>
    if admits filter account.provider then
      some { subject := .account index, live := account.enabled }
    else none)
  ++ backends.zipIdx.filterMap fun (backend, index) =>
    match backend.kind with
    | .account | .pool => none
    | _ => if filter.isSome then none else some { subject := .backend index, live := true }

structure Stored where
  windows : Bool
  readAt : Option Int
  error : Option String
  deriving DecidableEq, Repr

def unread : Stored := { windows := false, readAt := none, error := none }

inductive Outcome where
  | read
  | skippedRecent
  | notReported
  | disabled
  | unavailable
  deriving DecidableEq, Repr

structure Result where
  outcome : Outcome
  reason : Option String
  contacted : Bool
  stored : Stored
  deriving DecidableEq, Repr

def settle (outcome : Outcome) (stored : Stored) (reason : Option String := none) : Result :=
  { outcome, reason, contacted := false, stored }

/-- A contact that fails keeps the windows already stored, records why, and
still counts as the latest read, so the next automatic read waits out the
window. Usage is never invented from a failure. -/
def contact (now : Int) (response : Response) (stored : Stored) : Result :=
  match response.error with
  | none =>
    { outcome := .read, reason := none, contacted := true
      stored := { windows := true, readAt := some now, error := none } }
  | some error =>
    { outcome := .unavailable, reason := some error, contacted := true
      stored := { stored with readAt := some now, error := some error } }

def throttled (trigger : Trigger) (now : Int) (response : Response) (stored : Stored) : Result :=
  if shouldRead trigger now stored.readAt then contact now response stored
  else settle .skippedRecent stored

/-- The read of one planned subject. A disabled account or backend, a backend
with no usage source and a key that does not resolve are settled without
contacting the provider. -/
def read (backends : List Backend) (job : Job) (trigger : Trigger) (now : Int)
    (response : Response) (stored : Stored) : Result :=
  if !job.live then settle .disabled stored
  else match job.subject with
    | .account _ => throttled trigger now response stored
    | .backend index =>
      match backends[index]? with
      | none => settle .disabled stored
      | some backend =>
        if !backend.enabled then settle .disabled stored
        else match backend.kind with
          | .keyed => throttled trigger now response stored
          | .unkeyed => settle .unavailable stored (some "no key")
          | _ => settle .notReported stored

theorem backends_do_not_choose_the_accounts_read (accounts : List Account)
    (backends others : List Backend) (filter : Option Provider) :
    (plan accounts backends filter).filter (fun job => job.subject matches .account _)
      = (plan accounts others filter).filter (fun job => job.subject matches .account _) := by
  have none_of (list : List Backend) :
      ((list.zipIdx.filterMap fun (backend, index) =>
        match backend.kind with
        | .account | .pool => none
        | _ => if filter.isSome then none
          else some ({ subject := .backend index, live := true } : Job)).filter
        (fun job => job.subject matches .account _)) = [] := by
    rw [List.filter_eq_nil_iff]
    intro job member
    simp only [List.mem_filterMap] at member
    obtain ⟨⟨backend, index⟩, _, produced⟩ := member
    dsimp only at produced
    cases kind : backend.kind
    case account => simp [kind] at produced
    case pool => simp [kind] at produced
    all_goals
      simp only [kind] at produced
      split at produced
      · simp at produced
      · simp only [Option.some.injEq] at produced
        simp [← produced]
  simp only [plan, List.filter_append, none_of]

theorem a_provider_read_leaves_out_account_free_backends (accounts : List Account)
    (backends : List Backend) (provider : Provider) (job : Job)
    (planned : job ∈ plan accounts backends (some provider)) :
    ∃ index, job.subject = .account index := by
  simp only [plan, List.mem_append, List.mem_filterMap] at planned
  rcases planned with ⟨⟨account, index⟩, _, produced⟩ | ⟨⟨backend, index⟩, _, produced⟩
  · dsimp only at produced
    split at produced
    · simp only [Option.some.injEq] at produced
      exact ⟨index, by simp [← produced]⟩
    · exact absurd produced (by simp)
  · dsimp only at produced
    cases kind : backend.kind <;> simp [kind] at produced

theorem a_disabled_subject_is_never_contacted (backends : List Backend) (job : Job)
    (trigger : Trigger) (now : Int) (response : Response) (stored : Stored)
    (off : job.live = false) :
    read backends job trigger now response stored = settle .disabled stored := by
  simp [read, off]

theorem a_skipped_read_stores_nothing (trigger : Trigger) (now : Int) (response : Response)
    (stored : Stored) :
    (throttled trigger now response stored).contacted = false →
      (throttled trigger now response stored).stored = stored := by
  unfold throttled
  split
  · cases response <;> simp [contact, Response.error]
  · intro _
    rfl

theorem only_a_contact_changes_what_is_stored (backends : List Backend) (job : Job)
    (trigger : Trigger) (now : Int) (response : Response) (stored : Stored) :
    (read backends job trigger now response stored).contacted = false →
      (read backends job trigger now response stored).stored = stored := by
  unfold read
  repeat' split
  all_goals first
    | exact fun _ => rfl
    | exact a_skipped_read_stores_nothing trigger now response stored

theorem a_cached_read_contacts_no_one (backends : List Backend) (job : Job) (now : Int)
    (response : Response) (stored : Stored) :
    (read backends job .cached now response stored).contacted = false := by
  unfold read
  repeat' split
  all_goals simp [settle, throttled, shouldRead]

theorem a_failed_contact_keeps_the_stored_windows (now : Int) (response : Response)
    (stored : Stored) (error : String) (failed : response.error = some error) :
    (contact now response stored).stored.windows = stored.windows
      ∧ (contact now response stored).stored.error = some error := by
  simp [contact, failed]

theorem any_contact_holds_off_the_next_automatic_read (now later : Int) (response : Response)
    (stored : Stored) (soon : later - now < 300) (next : Response) :
    (throttled .open later next (contact now response stored).stored).contacted = false := by
  have recent : ¬ later - now ≥ 300 := by omega
  unfold contact
  split <;> simp [throttled, shouldRead, settle, recent]

end UsageRead
