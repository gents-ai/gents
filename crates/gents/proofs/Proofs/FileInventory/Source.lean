import Proofs.CanonicalOutput.Hydration
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

namespace FileInventory

structure Request where
  key : String
  peer : String
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

structure AppliedPairingRoute where
  peer : String
  requester : String
  node : String
  deriving DecidableEq, Repr

structure VerifiedActiveMembership where
  network : String
  member : String
  deriving DecidableEq, Repr

structure Document where
  collection : CanonicalOutput.Hydration.Collection
  id : Nat
  deriving DecidableEq, Repr

def Request.authorizationScope (r : Request) : CanonicalOutput.Hydration.AuthorizationScope :=
  ⟨r.peer, r.requester, r.node, r.session, r.nativeSession⟩

/-- The existing native ACP owner observes this exact physical root for the
authenticated enrolled reader in the source transaction. Permission comes from
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
  appliedPairingRoutes : Finset AppliedPairingRoute
  selectedNetwork : String
  verifiedActiveMemberships : Finset VerifiedActiveMembership
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

def verifiedMembership (cat : Catalog) (r : Request) : VerifiedActiveMembership :=
  { network := cat.selectedNetwork, member := r.requester }

def appliedPairingRoute (r : Request) : AppliedPairingRoute :=
  { peer := r.peer, requester := r.requester, node := r.node }

/-- Resolve one source root before interpreting its canonical requester. A
second physical root for the logical scope is ambiguous even if one is readable.
Canonical-author admission retains its existing enrollment contract. Another
reader needs the exact native ACP observation, including the physical root;
membership and pairing alone cannot supply it. -/
def sharedSourceSession (cat : Catalog) (r : Request) : Prop :=
  ∃ source ∈ cat.sessions,
    source.node = r.node ∧ source.session = r.session ∧ source.nativeSession = r.nativeSession ∧
    (source.requester = r.requester ∨
      cat.sessionReads.any (fun observation => observation.matches r source) = true) ∧
    ∀ other ∈ cat.sessions, other.node = r.node → other.session = r.session → other = source

instance (cat : Catalog) (r : Request) : Decidable (sharedSourceSession cat r) := by
  unfold sharedSourceSession
  infer_instance

def authorizedSourceSession (cat : Catalog) (r : Request) : Prop :=
  ownedSession r ∈ cat.sessions ∨ sharedSourceSession cat r

instance (cat : Catalog) (r : Request) : Decidable (authorizedSourceSession cat r) := by
  unfold authorizedSourceSession
  infer_instance

theorem owned_session_authorized (cat : Catalog) (r : Request)
    (owned : ownedSession r ∈ cat.sessions) : authorizedSourceSession cat r :=
  Or.inl owned

def admits (cat : Catalog) (r : Request) : Prop :=
  appliedPairingRoute r ∈ cat.appliedPairingRoutes ∧
  verifiedMembership cat r ∈ cat.verifiedActiveMemberships ∧
  authorizedSourceSession cat r

instance (cat : Catalog) (r : Request) : Decidable (admits cat r) := by
  unfold admits
  infer_instance

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
