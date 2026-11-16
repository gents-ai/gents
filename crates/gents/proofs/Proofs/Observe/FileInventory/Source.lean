import Proofs.Session.CanonicalOutput.Hydration
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

namespace FileInventory

structure Request where
  key : String
  requester : String
  node : String
  session : String
  /-- Zero `nativeSession` is reserved for abstract enrollment fixtures which
  never install closure input. -/
  nativeSession : SessionId := 0
  deriving DecidableEq, Repr

structure SessionOwner where
  session : String
  nativeSession : SessionId
  requester : String
  node : String
  deriving DecidableEq, Repr

structure Document where
  collection : CanonicalOutput.Hydration.Collection
  id : Nat
  deriving DecidableEq, Repr

def Request.authorizationScope (r : Request) : CanonicalOutput.Hydration.AuthorizationScope :=
  ⟨r.requester, r.node, r.session, r.nativeSession⟩

/-- The existing native ACP owner observes this exact physical root for the
authenticated reader in the source transaction. Permission comes from
the explicit read-visibility result, including public reads with unknown
ownership. Authored session fields select data; they do not authorize related
documents or images. Those observations must retain their own reader binding.
This is a native read fact, not a grant store or a client-authored claim. -/
structure SessionReadObservation where
  scope : CanonicalOutput.Hydration.AuthorizationScope
  source : SessionOwner
  visible : Bool
  deriving DecidableEq, Repr

def SessionReadObservation.matches (observation : SessionReadObservation)
    (r : Request) (source : SessionOwner) : Bool :=
  observation.scope == r.authorizationScope && observation.source == source &&
    observation.visible && source.nativeSession != 0

theorem invisible_root_does_not_authorize (observation : SessionReadObservation)
    (r : Request) (source : SessionOwner) (hidden : observation.visible = false) :
    observation.matches r source = false := by
  simp [SessionReadObservation.matches, hidden]

structure ClosureInput where
  request : Request
  bases : List CanonicalOutput.Hydration.AuthorizedBase
  roots : List CanonicalOutput.DocId
  requirements : List CanonicalOutput.Hydration.TerminalRequirement
  messages : List CanonicalOutput.MessageEnvelope
  segments : List CanonicalOutput.Segment
  access : List CanonicalOutput.Hydration.ProvenanceAccess
  deniedHeaders : List CanonicalOutput.DocId
  deniedSegments : List CanonicalOutput.DocId
  dependencyDenials : List CanonicalOutput.DependencyDenial := []
  effects : List CanonicalOutput.Hydration.EffectRow := []
  deriving DecidableEq

structure Catalog where
  sessions : Finset SessionOwner
  closureInputs : List ClosureInput
  sessionReads : List SessionReadObservation := []
  deriving DecidableEq

/-- Whether the terminal write that publishes a tool's inventory projection
committed. A losing or rolled-back terminal publishes nothing. -/
inductive TerminalWriteResult where
  | committed
  | failed
  | notAttempted
  deriving DecidableEq, Repr

def ownedSession (r : Request) : SessionOwner :=
  { session := r.session, nativeSession := r.nativeSession,
    requester := r.requester, node := r.node }

def closureInputFor (inputs : List ClosureInput) (r : Request) : Option ClosureInput :=
  match inputs.filter (fun input => input.request == r) with
  | [input] => some input
  | _ => none

def canonicalDocument (key : CanonicalOutput.Hydration.DocumentKey) : Document :=
  ⟨key.collection, key.id⟩

def buildDocuments (input : ClosureInput) :
    Except CanonicalOutput.Hydration.Error (Finset Document) := do
  let manifest ← CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
    input.request.nativeSession
    input.bases input.roots input.requirements input.messages input.segments input.access
    input.deniedHeaders input.deniedSegments input.dependencyDenials input.effects
  pure (manifest.map canonicalDocument).toFinset

/-- `none` means absent/ambiguous input or failed canonical authorization; a
successful empty manifest is `some ∅` and remains distinguishable. -/
def selectedDocuments (cat : Catalog) (r : Request) : Option (Finset Document) :=
  (closureInputFor cat.closureInputs r).bind fun input =>
    (buildDocuments input).toOption

theorem selected_documents_have_exact_input (cat : Catalog) (r : Request)
    (documents : Finset Document) (h : selectedDocuments cat r = some documents) :
    ∃ input ∈ cat.closureInputs, input.request = r ∧ buildDocuments input = .ok documents := by
  unfold selectedDocuments at h
  cases hi : closureInputFor cat.closureInputs r with
  | none => simp [hi] at h
  | some input =>
      rw [hi] at h
      change (buildDocuments input).toOption = some documents at h
      cases hb : buildDocuments input with
      | error error =>
          rw [hb] at h
          contradiction
      | ok docs =>
          rw [hb] at h
          have hdocs : docs = documents := Option.some.inj h
          subst documents
          unfold closureInputFor at hi
          generalize heq : cat.closureInputs.filter (fun candidate => candidate.request == r) =
            filtered at hi
          cases filtered with
          | nil => contradiction
          | cons found rest =>
              cases rest with
              | nil =>
                  simp only [Option.some.injEq] at hi
                  subst found
                  have hm : input ∈ cat.closureInputs.filter
                      (fun candidate => candidate.request == r) := by simp [heq]
                  have parts := List.mem_filter.mp hm
                  exact ⟨input, parts.1, by simpa using parts.2, hb⟩
              | cons head tail => contradiction

/-- Closure failures caused by rows that are not yet readable leave the
inventory unknown; every other failure rejects it. -/
def closureErrorIsLoading : CanonicalOutput.Hydration.Error → Bool
  | .headerUnavailable | .provenanceMissing _ => true
  | .reference (.lookup .unavailable) => true
  | .reference (.extent (.missingOrdinal _)) => true
  | .terminal .missingHeader => true
  | _ => false

end FileInventory
