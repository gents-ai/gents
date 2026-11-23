/-! Native file tools reach a path beneath their tool root through directory
handles. The root is opened once per operation; each component is looked up
in the directory handle the previous step returned, by a call that does not
follow a symbolic link at that name. A link's text is read and spliced in
front of the remaining components and resolved again beneath the root, so the
kernel never follows a link on the tools' behalf. `..` returns to a handle
already held and is refused at the root. The operation then acts on the entry
of the last name in the last held handle. The interleaving with a process that
changes the namespace meanwhile is `tla/PathNamespaceRace.tla`.

External premise: a lookup of a single component relative to a directory
handle (`openat` with `O_NOFOLLOW`, `readlinkat`, `mkdirat`, `renameat`, with a
name holding no `/`) acts on the entry of that name in the directory object
the handle refers to at the moment of the call and does not follow a link at
that name, and a directory handle keeps referring to the same directory object
when that directory is renamed or replaced. -/

namespace PathNamespace

/-- One component of a requested path or of a link's text. `.` never reaches
the walk. -/
inductive Component where
  | name
  | parent
  deriving DecidableEq, Repr

/-- What one lookup in the held directory reports for a name, without
following a link there. A link is classified by its text: relative to the held
directory, absolute and naming a path beneath the root, or absolute and naming
a path outside it. -/
inductive Observed where
  | directory
  | nonDirectory
  | missing
  | linkRelative
  | linkBeneathRoot
  | linkOutsideRoot
  deriving DecidableEq, Repr

/-- What the walk does with one component. -/
inductive Step where
  /-- Drop the held handle and continue in the one below it. -/
  | ascend
  /-- Hold the directory handle the lookup opened. -/
  | descend
  /-- The target is this existing entry of the held directory. -/
  | land
  /-- The target is this absent entry of the held directory, to be created. -/
  | landMissing
  /-- This directory and the components after it are absent and are created
  beneath the held directory. -/
  | pendingCreate
  /-- Continue with the link's text in front of the remaining components, from
  the held directory. -/
  | followRelative
  /-- Continue with the link's text, made relative to the root, from the root
  handle. -/
  | followFromRoot
  | refuseEscape
  | refuseNotDirectory
  | refuseNotFound
  | refuseDanglingLink
  | refuseLoop
  deriving DecidableEq, Repr

/-- The decision for one component. `atRoot`: the root is the only held
handle. `last`: no component follows. `create`: the operation creates its
target. `viaLink`: the component came from a link's text. `hopsLeft`: another
link may still be followed.

Decisions: `..` never leaves the root; a link is never opened, only read and
resolved again beneath the root, and one whose text leaves the root is
refused; an absent component is created only when it was named by the request
itself, so a link whose target is absent (a dangling link) is refused rather
than created through. -/
def step (component : Component) (atRoot last create viaLink hopsLeft : Bool)
    (observed : Observed) : Step :=
  match component with
  | .parent => if atRoot then .refuseEscape else .ascend
  | .name =>
    match observed with
    | .directory => if last then .land else .descend
    | .nonDirectory => if last then .land else .refuseNotDirectory
    | .missing =>
      if !create then .refuseNotFound
      else if viaLink then .refuseDanglingLink
      else if last then .landMissing
      else .pendingCreate
    | .linkOutsideRoot => .refuseEscape
    | .linkRelative => if hopsLeft then .followRelative else .refuseLoop
    | .linkBeneathRoot => if hopsLeft then .followFromRoot else .refuseLoop

def Step.opensEntry : Step → Bool
  | .descend | .land | .landMissing | .pendingCreate => true
  | _ => false

theorem the_root_has_no_parent (last create viaLink hopsLeft : Bool) (observed : Observed) :
    step .parent true last create viaLink hopsLeft observed = .refuseEscape := rfl

theorem a_link_out_of_the_root_is_refused (atRoot last create viaLink hopsLeft : Bool) :
    step .name atRoot last create viaLink hopsLeft .linkOutsideRoot = .refuseEscape := rfl

/-- A link is never handed to the kernel to open or create through. -/
theorem a_link_is_never_opened (atRoot last create viaLink hopsLeft : Bool) :
    (step .name atRoot last create viaLink hopsLeft .linkRelative).opensEntry = false ∧
    (step .name atRoot last create viaLink hopsLeft .linkBeneathRoot).opensEntry = false ∧
    (step .name atRoot last create viaLink hopsLeft .linkOutsideRoot).opensEntry = false := by
  cases hopsLeft <;> exact ⟨rfl, rfl, rfl⟩

theorem a_dangling_link_is_not_created_through (atRoot last hopsLeft : Bool) :
    step .name atRoot last true true hopsLeft .missing = .refuseDanglingLink := rfl

/-! The walk over an adversarial namespace. The kernel's answer to the `t`-th
lookup is any function of `t`, so another process may change the namespace
between any two lookups. -/

abbrev Dir := Nat

inductive Comp where
  | name (n : String)
  | parent
  deriving DecidableEq, Repr

/-- A link's text: relative to the held directory, or absolute and made
relative to the root (`none` when it names a path outside the root). -/
inductive Link where
  | relative (path : List Comp)
  | absolute (beneath : Option (List Comp))
  deriving Repr

/-- The kernel's answer to a lookup that does not follow a link; a directory
answer is the handle it opened. -/
inductive Answer where
  | directory (handle : Dir)
  | nonDirectory
  | missing
  | link (text : Link)
  deriving Repr

def Answer.observed : Answer → Observed
  | .directory _ => .directory
  | .nonDirectory => .nonDirectory
  | .missing => .missing
  | .link (.relative _) => .linkRelative
  | .link (.absolute (some _)) => .linkBeneathRoot
  | .link (.absolute none) => .linkOutsideRoot

abbrev Kernel := Nat → Dir → String → Answer

structure State where
  /-- Held directory handles, the innermost first. -/
  stack : List Dir
  /-- Remaining components, each marked when it came from a link's text. -/
  queue : List (Comp × Bool)
  hops : Nat
  clock : Nat

inductive Outcome where
  | refused (step : Step)
  /-- The target is the innermost held directory. -/
  | held (stack : List Dir)
  /-- The target is `name` in the innermost held directory, with `missing`
  absent components to create beneath it. -/
  | entry (stack : List Dir) (name : String) (missing : List String)

def namesOnly : List (Comp × Bool) → Option (List String)
  | [] => some []
  | (.name n, _) :: rest => (namesOnly rest).map (n :: ·)
  | (.parent, _) :: _ => none

/-- The walk's continuation after the kernel answered a name's lookup;
`recurse` continues the walk from a state. -/
def afterName (root : Dir) (create : Bool) (maxHops : Nat) (recurse : State → Outcome)
    (s : State) (rest : List (Comp × Bool)) (via : Bool) (n : String) (answer : Answer) :
    Outcome :=
  match step .name (decide (s.stack.length ≤ 1)) rest.isEmpty create via
      (decide (s.hops < maxHops)) answer.observed, answer with
  | .descend, .directory d =>
    recurse { s with stack := d :: s.stack, queue := rest, clock := s.clock + 1 }
  | .land, _ => .entry s.stack n []
  | .landMissing, _ => .entry s.stack n []
  | .pendingCreate, _ =>
    match namesOnly rest with
    | some names => .entry s.stack n names
    | none => .refused .refuseNotFound
  | .followRelative, .link (.relative path) =>
    recurse { s with queue := path.map (·, true) ++ rest, hops := s.hops + 1, clock := s.clock + 1 }
  | .followFromRoot, .link (.absolute (some path)) =>
    recurse { s with stack := [root], queue := path.map (·, true) ++ rest, hops := s.hops + 1,
                     clock := s.clock + 1 }
  | r, _ => .refused r

def walk (kernel : Kernel) (root : Dir) (create : Bool) (maxHops : Nat) :
    Nat → State → Outcome
  | 0, _ => .refused .refuseLoop
  | fuel + 1, s =>
    match s.queue with
    | [] => .held s.stack
    | (.parent, via) :: rest =>
      match step .parent (decide (s.stack.length ≤ 1)) rest.isEmpty create via
          (decide (s.hops < maxHops)) .missing with
      | .ascend => walk kernel root create maxHops fuel { s with stack := s.stack.tail, queue := rest }
      | r => .refused r
    | (.name n, via) :: rest =>
      afterName root create maxHops (walk kernel root create maxHops fuel) s rest via n
        (kernel s.clock (s.stack.headD root) n)

/-- Each held handle is one the kernel returned for a name looked up in the
handle below it, and the outermost is the root. -/
def Chained (kernel : Kernel) (root : Dir) : List Dir → Prop
  | [] => False
  | [d] => d = root
  | d :: p :: rest => (∃ t n, kernel t p n = .directory d) ∧ Chained kernel root (p :: rest)

/-- Every handle an outcome holds descends from the root handle. -/
def Outcome.Beneath (kernel : Kernel) (root : Dir) : Outcome → Prop
  | .refused _ => True
  | .held stack => Chained kernel root stack
  | .entry stack _ _ => Chained kernel root stack

theorem chained_cons (kernel : Kernel) (root : Dir) (stack : List Dir)
    (h : Chained kernel root stack) : ∃ p tail, stack = p :: tail := by
  match stack, h with
  | p :: tail, _ => exact ⟨p, tail, rfl⟩

theorem chained_tail (kernel : Kernel) (root : Dir) (stack : List Dir)
    (h : Chained kernel root stack) (notRoot : ¬ stack.length ≤ 1) :
    Chained kernel root stack.tail := by
  match stack, h with
  | _ :: _ :: _, h => exact h.2
  | [_], _ => simp at notRoot

theorem afterName_beneath (kernel : Kernel) (root : Dir) (create : Bool) (maxHops : Nat)
    (recurse : State → Outcome)
    (hrec : ∀ s', Chained kernel root s'.stack → (recurse s').Beneath kernel root)
    (s : State) (rest : List (Comp × Bool)) (via : Bool) (n : String)
    (hs : Chained kernel root s.stack) :
    (afterName root create maxHops recurse s rest via n
      (kernel s.clock (s.stack.headD root) n)).Beneath kernel root := by
  obtain ⟨p, tail, hst⟩ := chained_cons kernel root s.stack hs
  have hans : kernel s.clock p n = kernel s.clock (s.stack.headD root) n := by
    simp [hst]
  generalize kernel s.clock (s.stack.headD root) n = answer at hans ⊢
  cases answer with
  | directory d =>
    cases hl : rest.isEmpty
    · simp only [afterName, Answer.observed, step, hl, Bool.false_eq_true, ↓reduceIte]
      apply hrec
      show Chained kernel root (d :: s.stack)
      rw [hst]
      exact ⟨⟨s.clock, n, hans⟩, hst ▸ hs⟩
    · simp only [afterName, Answer.observed, step, hl, ↓reduceIte]
      exact hs
  | nonDirectory =>
    cases hl : rest.isEmpty <;>
      simp only [afterName, Answer.observed, step, hl, Bool.false_eq_true, ↓reduceIte,
        Outcome.Beneath] <;> trivial
  | missing =>
    cases create <;> cases via <;> cases hl : rest.isEmpty <;>
      simp only [afterName, Answer.observed, step, hl, Bool.false_eq_true, Bool.not_false,
        Bool.not_true, ↓reduceIte, Outcome.Beneath]
    all_goals first
      | trivial
      | exact hs
      | (cases namesOnly rest <;> first | trivial | exact hs)
  | link text =>
    cases text with
    | relative path =>
      by_cases hh : s.hops < maxHops
      · simp only [afterName, Answer.observed, step, hh, decide_true, ↓reduceIte]
        exact hrec _ hs
      · simp only [afterName, Answer.observed, step, hh, decide_false, Bool.false_eq_true,
          ↓reduceIte, Outcome.Beneath]
    | absolute beneath =>
      cases beneath with
      | none => simp only [afterName, Answer.observed, step, Outcome.Beneath]
      | some path =>
        by_cases hh : s.hops < maxHops
        · simp only [afterName, Answer.observed, step, hh, decide_true, ↓reduceIte]
          exact hrec _ rfl
        · simp only [afterName, Answer.observed, step, hh, decide_false, Bool.false_eq_true,
            ↓reduceIte, Outcome.Beneath]

/-- Whatever another process does to the namespace between lookups, every
handle the walk holds when it reaches its target descends from the root
handle by lookups the kernel answered; the target is an entry of the
innermost of them. -/
theorem walk_holds_only_handles_beneath_the_root (kernel : Kernel) (root : Dir)
    (create : Bool) (maxHops : Nat) :
    ∀ (fuel : Nat) (s : State), Chained kernel root s.stack →
      (walk kernel root create maxHops fuel s).Beneath kernel root := by
  intro fuel
  induction fuel with
  | zero => intro s _; simp [walk, Outcome.Beneath]
  | succ fuel ih =>
    intro s hs
    match hq : s.queue with
    | [] => simpa [walk, hq, Outcome.Beneath] using hs
    | (.parent, via) :: rest =>
      by_cases hroot : s.stack.length ≤ 1
      · simp [walk, hq, step, hroot, Outcome.Beneath]
      · simp only [walk, hq, step, hroot, decide_false, Bool.false_eq_true, ↓reduceIte]
        exact ih _ (chained_tail kernel root s.stack hs hroot)
    | (.name n, via) :: rest =>
      simp only [walk, hq]
      exact afterName_beneath kernel root create maxHops _ ih s rest via n hs

end PathNamespace
