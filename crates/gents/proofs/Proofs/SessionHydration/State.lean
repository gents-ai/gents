import Proofs.CanonicalOutput.Hydration
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# Session hydration state

The model separates admission from reference closure. Pairing, membership, and
session ownership admit a request; its unique `ClosureInput` is then executed by
`CanonicalOutput.Hydration.buildManifest` against exact owner evidence. It is
not a broad session inventory. In particular,
a retained fork dependency may belong to an origin session without granting that
session. Pairing observations are read-only admission inputs; hydration owns
deliveries and terminal outcomes.
-/

namespace SessionHydration

structure Request where
  key : String
  peer : String
  requester : String
  agent : String
  session : String
  /-- Native identity supplied by the session owner. Zero is reserved for
  abstract enrollment fixtures which never install closure input. -/
  nativeSession : SessionId := 0
  deriving DecidableEq, Repr

/-- Read-only ownership projection of canonical AgentSession. This is not a second
durable session document. Hydration requires a present exact requester; absent or
invalid requester metadata never becomes wildcard membership. -/
structure SessionOwner where
  session : String
  nativeSession : SessionId
  requester : String
  agent : String
  deriving DecidableEq, Repr

/-- A locally desired pairing whose exact requester/agent filter was applied for this peer. -/
structure AppliedPairingRoute where
  peer : String
  requester : String
  agent : String
  deriving DecidableEq, Repr

/-- An active membership already verified against the selected network root. -/
structure VerifiedActiveMembership where
  network : String
  member : String
  deriving DecidableEq, Repr

structure Document where
  collection : CanonicalOutput.Hydration.Collection
  id : Nat
  deriving DecidableEq, Repr

def Request.authorizationScope (r : Request) : CanonicalOutput.Hydration.AuthorizationScope :=
  ⟨r.peer, r.requester, r.agent, r.session, r.nativeSession⟩

/-- Inputs retained at the consumed boundary. Selection executes the canonical
builder; the catalog never accepts a caller-supplied manifest. -/
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
  deriving DecidableEq

structure Catalog where
  appliedPairingRoutes : Finset AppliedPairingRoute
  selectedNetwork : String
  verifiedActiveMemberships : Finset VerifiedActiveMembership
  sessions : Finset SessionOwner
  closureInputs : List ClosureInput
  deriving DecidableEq

inductive Outcome where
  | served
  | rejected
  deriving DecidableEq, Repr

/-- Result after the bounded delivery attempt budget is consumed. An
`indeterminate` result may follow partial transport side effects; it only says
that delivery of the complete selected set was not confirmed. -/
inductive DeliveryResult where
  | confirmed
  | indeterminate
  deriving DecidableEq, Repr

/-- Result of committing the signed terminal hydration receipt. Delivery and
terminal persistence are separate effects in the Rust reconciler. -/
inductive TerminalWriteResult where
  | committed
  | failed
  | notAttempted
  deriving DecidableEq, Repr

structure Terminal where
  key : String
  outcome : Outcome
  servedDocuments : Finset Document
  deriving DecidableEq

structure State where
  attempted : Finset Document
  confirmedDelivered : Finset Document
  terminals : Finset Terminal
  deriving DecidableEq

def transcriptCollections : Finset CanonicalOutput.Hydration.Collection :=
  [.agentRequest, .agentMessage, .agentToolCall,
   .agentOutputSegment, .compactionEntry].toFinset

theorem collection_is_transcript
    (collection : CanonicalOutput.Hydration.Collection) :
    collection ∈ transcriptCollections := by
  cases collection <;> simp [transcriptCollections]

def ownedSession (r : Request) : SessionOwner :=
  { session := r.session, nativeSession := r.nativeSession,
    requester := r.requester, agent := r.agent }

def verifiedMembership (cat : Catalog) (r : Request) : VerifiedActiveMembership :=
  { network := cat.selectedNetwork, member := r.requester }

def appliedPairingRoute (r : Request) : AppliedPairingRoute :=
  { peer := r.peer, requester := r.requester, agent := r.agent }

def admits (cat : Catalog) (r : Request) : Prop :=
  appliedPairingRoute r ∈ cat.appliedPairingRoutes ∧
  verifiedMembership cat r ∈ cat.verifiedActiveMemberships ∧
  ownedSession r ∈ cat.sessions

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
    input.deniedHeaders input.deniedSegments input.dependencyDenials
  pure (manifest.map canonicalDocument).toFinset

/-- `none` means absent/ambiguous input or failed canonical authorization. A
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

def terminalFor (st : State) (key : String) : Prop :=
  ∃ terminal ∈ st.terminals, terminal.key = key

instance (st : State) (key : String) : Decidable (terminalFor st key) := by
  unfold terminalFor
  infer_instance

def terminal (r : Request) (outcome : Outcome) (servedDocuments : Finset Document) : Terminal :=
  { key := r.key, outcome, servedDocuments }

def applyStep (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult) : State :=
  if terminalFor st r.key then st
  else if admits cat r then
    match selectedDocuments cat r with
    | none =>
      match terminalWrite with
      | .committed => { st with terminals := insert (terminal r .rejected ∅) st.terminals }
      | .failed | .notAttempted => st
    | some docs =>
      match delivery with
      | .confirmed =>
        match terminalWrite with
        | .committed =>
          { st with
            attempted := st.attempted ∪ docs
            confirmedDelivered := st.confirmedDelivered ∪ docs
            terminals := insert (terminal r .served docs) st.terminals }
        | .failed | .notAttempted =>
          { st with
            attempted := st.attempted ∪ docs
            confirmedDelivered := st.confirmedDelivered ∪ docs }
      | .indeterminate => { st with attempted := st.attempted ∪ docs }
  else
    match terminalWrite with
    | .committed =>
      { st with terminals := insert (terminal r .rejected ∅) st.terminals }
    | .failed => st
    | .notAttempted => st

end SessionHydration
