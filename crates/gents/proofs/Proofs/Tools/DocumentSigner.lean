/-! A query tool may list the reserved `_signer` field. Its value is read from
the signature headers of the document's commits, never from a stored field:
it is the one identity that signed every commit, and a document with an
unsigned commit, or with commits signed by two identities, has none. A filter
on `_signer` keeps exactly the documents whose signer equals it. -/

namespace DocumentSigner

/-- The name a surface lists; no create may write it. -/
def reservedField : String := "_signer"

/-- Who signed one commit, as the store's signature header names it. A commit
is one composite block: DefraDB writes one per create or update and signs it
with the writer's key, while an update's field blocks carry no signature, so
only composite blocks name the writer. -/
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

/-- The identity every commit names, or none. -/
def signer : List Commit → Option Signer
  | [] => none
  | first :: rest =>
      match first.signer with
      | some identity => if rest.all (·.signer == some identity) then some identity else none
      | none => none

def admits (filter : Option Signer) (commits : List Commit) : Bool :=
  match filter with
  | none => true
  | some wanted => signer commits == some wanted

theorem a_signer_signed_every_commit (commits : List Commit) (identity : Signer)
    (signed : signer commits = some identity) :
    ∀ commit ∈ commits, commit.signer = some identity := by
  cases commits with
  | nil => simp [signer] at signed
  | cons first rest =>
    simp only [signer] at signed
    split at signed
    · rename_i head headSigned
      split at signed
      · rename_i all
        cases signed
        intro commit member
        rcases List.mem_cons.mp member with h | h
        · subst h; exact headSigned
        · simpa using (List.all_eq_true.mp all) commit h
      · cases signed
    · cases signed

theorem an_unsigned_commit_leaves_no_signer (commits : List Commit)
    (unsigned : Commit.unsigned ∈ commits) : signer commits = none := by
  cases found : signer commits with
  | none => rfl
  | some identity =>
    have := a_signer_signed_every_commit commits identity found _ unsigned
    simp [Commit.signer] at this

theorem a_filter_admits_only_its_signer (filter : Signer) (commits : List Commit)
    (admitted : admits (some filter) commits = true) : signer commits = some filter := by
  simpa [admits] using admitted

end DocumentSigner
