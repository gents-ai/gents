/-! A datastore query surface may list the reserved `_signer` field. Its value
is the document's provenance, read from the verified signatures of its
versions and never from a stored field (`gents_store::provenance`): every
identity that signed a version, and whether some version is unattributed. A
filter on `_signer` keeps exactly the documents that identity signed every
version of, so a pack can accept only what its own node or its own user's page
wrote. No create may write the field. -/

namespace DocumentSigner

/-- The name a surface lists; no create may write it. -/
def reservedField : String := "_signer"

/-- Who signed one version, as its verified signature names it. -/
inductive Commit where
  | unsigned
  | reader
  | other
  deriving DecidableEq, Repr

inductive Signer where
  | reader
  | other
  deriving DecidableEq, Repr

def Commit.signer : Commit → Option Signer
  | .unsigned => none
  | .reader => some .reader
  | .other => some .other

/-- `identity` signed some version. -/
def wrote (identity : Signer) (commits : List Commit) : Bool :=
  commits.any fun commit => decide (commit.signer = some identity)

/-- Some version carries no signature, or there is no readable version. -/
def unsigned (commits : List Commit) : Bool :=
  commits.isEmpty || commits.any fun commit => decide (commit = .unsigned)

/-- `identity` signed every version, and there is one. -/
def signedOnlyBy (identity : Signer) (commits : List Commit) : Bool :=
  !commits.isEmpty && commits.all fun commit => decide (commit.signer = some identity)

def admits (filter : Option Signer) (commits : List Commit) : Bool :=
  match filter with
  | none => true
  | some identity => signedOnlyBy identity commits

theorem a_filter_admits_only_documents_its_signer_wrote (identity : Signer)
    (commits : List Commit) (admitted : admits (some identity) commits = true) :
    ∀ commit ∈ commits, commit.signer = some identity := by
  intro commit member
  simp only [admits, signedOnlyBy, Bool.and_eq_true, List.all_eq_true,
    decide_eq_true_eq] at admitted
  exact admitted.2 commit member

theorem an_unsigned_version_is_never_admitted (identity : Signer) (commits : List Commit)
    (unsignedVersion : Commit.unsigned ∈ commits) :
    admits (some identity) commits = false := by
  cases admitted : admits (some identity) commits with
  | false => rfl
  | true =>
    have := a_filter_admits_only_documents_its_signer_wrote identity commits admitted
      _ unsignedVersion
    simp [Commit.signer] at this

theorem a_filter_admits_only_what_its_signer_alone_wrote (identity other : Signer)
    (commits : List Commit) (distinct : other ≠ identity)
    (admitted : admits (some identity) commits = true) : wrote other commits = false := by
  have every := a_filter_admits_only_documents_its_signer_wrote identity commits admitted
  cases found : wrote other commits with
  | false => rfl
  | true =>
    simp only [wrote, List.any_eq_true, decide_eq_true_eq] at found
    obtain ⟨commit, member, signed⟩ := found
    rw [every commit member] at signed
    exact absurd (Option.some.inj signed).symm distinct

end DocumentSigner
