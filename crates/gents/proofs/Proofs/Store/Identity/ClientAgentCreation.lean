import Proofs.Identity.Household
import Lean

namespace Identity.ClientAgentCreation
open Lean Identity.Household

/-- The client-signed route is requested explicitly for Agent creates in an
HTTP desired-state plan. Other collections and updates retain their existing
route. These facts describe adapter capability and verified external results;
they are not an alternative database authorization policy. -/
structure Input where
  clientIntent : Bool
  agentCollection : Bool
  create : Bool
  http : Bool
  signerAvailable : Bool
  supported : Bool
  signerMatchesBearer : Bool
  inputMatchesGraph : Bool
  trustedSchema : Bool
  graphVerified : Bool
  transportReady : Bool
  nativeAuthorized : Bool
  deriving Repr

inductive Route where
  | native | signed | refuse
  deriving BEq, DecidableEq, Repr

def route (input : Input) : Route :=
  if !input.clientIntent || !input.agentCollection || !input.create then .native
  else if input.http && input.signerAvailable && input.supported && input.signerMatchesBearer &&
      input.inputMatchesGraph && input.trustedSchema && input.graphVerified && input.transportReady then .signed
  else .refuse

theorem required_create_never_falls_back (input : Input)
    (required : input.clientIntent = true) (agent : input.agentCollection = true)
    (create : input.create = true) : route input ≠ .native := by
  simp [route, required, agent, create] <;> split <;> decide

theorem signer_mismatch_refuses (input : Input)
    (required : input.clientIntent = true) (agent : input.agentCollection = true)
    (create : input.create = true) (mismatch : input.signerMatchesBearer = false) :
    route input = .refuse := by
  simp [route, required, agent, create, mismatch]

/-- Unknown commit is not modeled as a successful or aborted transaction.
The committed state is conditional on the native create gate and verified
client signature; ACP grants and their outcome are separate operations. -/
inductive CommitObservation where
  | aborted | unknown | committed
  deriving BEq, DecidableEq, Repr

def signedCreate (input : Input) (state : State) (signer bearer docId : String)
    (commit : CommitObservation) : Option State :=
  if route input != .signed || !input.nativeAuthorized || signer.isEmpty || signer != bearer then some state
  else match commit with
    | .aborted => some state
    | .unknown => none
    | .committed => some (directCreate state signer "agents" docId true true)

theorem unknown_commit_is_not_assumed (input : Input) (state : State) (signer docId : String)
    (signed : route input = .signed) (authorized : input.nativeAuthorized = true)
    (nonempty : signer.isEmpty = false) :
    signedCreate input state signer signer docId .unknown = none := by
  have sameRoute : (Route.signed != Route.signed) = false := by decide
  simp [signedCreate, signed, authorized, nonempty, sameRoute]

theorem fresh_signed_agent_is_owned_by_signer (input : Input) (state : State) (signer docId : String)
    (signed : route input = .signed) (authorized : input.nativeAuthorized = true)
    (nonempty : signer.isEmpty = false)
    (fresh : state.objects.any (fun object => object.resource == "agents" && object.id == docId) = false) :
    signedCreate input state signer signer docId .committed =
      some { state with objects := ⟨"agents", docId, signer⟩ :: state.objects } := by
  have sameRoute : (Route.signed != Route.signed) = false := by decide
  simp [signedCreate, signed, authorized, nonempty, directCreate, fresh, sameRoute]

theorem signed_create_adds_no_implicit_grants (input : Input) (state after : State)
    (signer bearer docId : String) (commit : CommitObservation)
    (result : signedCreate input state signer bearer docId commit = some after) :
    after.relations = state.relations := by
  unfold signedCreate at result
  split at result
  · cases result; rfl
  · cases commit with
    | aborted => cases result; rfl
    | unknown => simp at result
    | committed =>
      cases result
      exact direct_create_has_no_implicit_relations state signer "agents" docId true true

inductive GrantObservation where
  | incomplete | complete | unknown
  deriving BEq, DecidableEq, Repr

inductive Publication where
  | nativeRoute | unchanged | unknownCommit | committedIncomplete | committedComplete | committedGrantUnknown
  deriving BEq, DecidableEq, Repr

/-- A committed-but-incomplete outcome is a returned observation, not a
durable recovery marker. A caller retains intended grants; an unknown commit or
grant outcome cannot establish publication or justify blind replay. -/
def publication (input : Input) (commit : CommitObservation) (grants : GrantObservation) : Publication :=
  match route input with
  | .native => .nativeRoute
  | .refuse => .unchanged
  | .signed =>
    if !input.nativeAuthorized then .unchanged
    else match commit with
      | .aborted => .unchanged
      | .unknown => .unknownCommit
      | .committed => match grants with
        | .incomplete => .committedIncomplete
        | .complete => .committedComplete
        | .unknown => .committedGrantUnknown

theorem unknown_grant_remains_unknown (input : Input) (signed : route input = .signed)
    (authorized : input.nativeAuthorized = true) :
    publication input .committed .unknown = .committedGrantUnknown := by
  simp [publication, signed, authorized]

structure Case where
  name : String
  input : Input
  commit : CommitObservation
  grants : GrantObservation

def validInput : Input := ⟨true, true, true, true, true, true, true, true, true, true, true, true⟩

def cases : List Case :=
  [ ⟨"signed-commit", validInput, .committed, .complete⟩
  , ⟨"grant-failed", validInput, .committed, .incomplete⟩
  , ⟨"grant-unknown", validInput, .committed, .unknown⟩
  , ⟨"commit-unknown", validInput, .unknown, .unknown⟩
  , ⟨"transaction-aborted", validInput, .aborted, .complete⟩
  , ⟨"native-denied", { validInput with nativeAuthorized := false }, .committed, .complete⟩
  , ⟨"no-signer", { validInput with signerAvailable := false }, .committed, .complete⟩
  , ⟨"unsupported", { validInput with supported := false }, .committed, .complete⟩
  , ⟨"foreign-signer", { validInput with signerMatchesBearer := false }, .committed, .complete⟩
  , ⟨"mismatched-input", { validInput with inputMatchesGraph := false }, .committed, .complete⟩
  , ⟨"untrusted-schema", { validInput with trustedSchema := false }, .committed, .complete⟩
  , ⟨"invalid-graph", { validInput with graphVerified := false }, .committed, .complete⟩
  , ⟨"transport-failed", { validInput with transportReady := false }, .committed, .complete⟩
  , ⟨"non-http", { validInput with http := false }, .committed, .complete⟩
  , ⟨"agent-update", { validInput with create := false }, .committed, .complete⟩
  , ⟨"other-collection", { validInput with agentCollection := false }, .committed, .complete⟩
  , ⟨"ordinary-caller", { validInput with clientIntent := false }, .committed, .complete⟩ ]

def routeString : Route → String
  | .native => "native" | .signed => "signed" | .refuse => "refuse"

def commitString : CommitObservation → String
  | .aborted => "aborted" | .unknown => "unknown" | .committed => "committed"

def grantsString : GrantObservation → String
  | .incomplete => "incomplete" | .complete => "complete" | .unknown => "unknown"

def publicationString : Publication → String
  | .nativeRoute => "native_route" | .unchanged => "unchanged" | .unknownCommit => "unknown_commit"
  | .committedIncomplete => "committed_incomplete" | .committedComplete => "committed_complete"
  | .committedGrantUnknown => "committed_grant_unknown"

def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name), ("client_intent", toJson c.input.clientIntent),
   ("agent_collection", toJson c.input.agentCollection), ("create", toJson c.input.create),
   ("http", toJson c.input.http), ("signer_available", toJson c.input.signerAvailable),
   ("supported", toJson c.input.supported), ("signer_matches_bearer", toJson c.input.signerMatchesBearer),
   ("input_matches_graph", toJson c.input.inputMatchesGraph), ("trusted_schema", toJson c.input.trustedSchema),
   ("graph_verified", toJson c.input.graphVerified), ("transport_ready", toJson c.input.transportReady),
   ("native_authorized", toJson c.input.nativeAuthorized), ("commit", toJson (commitString c.commit)),
   ("grants", toJson (grantsString c.grants)), ("expected_route", toJson (routeString (route c.input))),
   ("expected_publication", toJson (publicationString (publication c.input c.commit c.grants)))]

def casesJson : String := (toJson (cases.map caseJson)).compress

/-- Arbitrary application collections are signable only with SDL the caller
explicitly supplied as its own authored input. A remote schema is an observation,
not authority to choose the bytes signed by the User. -/
inductive DocumentSchemaSource where
  | authored | remote
  deriving BEq, DecidableEq, Repr

structure DocumentCreateInput where
  source : DocumentSchemaSource
  singleCollection : Bool
  applicationCollection : Bool
  matchesLive : Bool
  heldSigner : Bool
  http : Bool
  deriving Repr

def documentCreateRoute (input : DocumentCreateInput) : Route :=
  if input.source == .authored && input.singleCollection && input.applicationCollection &&
      input.matchesLive && input.heldSigner && input.http then .signed else .refuse

theorem remote_document_schema_refused (input : DocumentCreateInput)
    (remote : input.source = .remote) : documentCreateRoute input = .refuse := by
  cases input with
  | mk source single application live signer http =>
    cases source with
    | authored => cases remote
    | remote => rfl

theorem document_create_has_no_native_fallback (input : DocumentCreateInput) :
    documentCreateRoute input ≠ .native := by
  unfold documentCreateRoute
  split <;> decide

structure DocumentCreateCase where
  name : String
  input : DocumentCreateInput

def validDocumentInput : DocumentCreateInput := ⟨.authored, true, true, true, true, true⟩

def documentCreateCases : List DocumentCreateCase :=
  [ ⟨"authored", validDocumentInput⟩
  , ⟨"remote", { validDocumentInput with source := .remote }⟩
  , ⟨"multiple-collections", { validDocumentInput with singleCollection := false }⟩
  , ⟨"protected", { validDocumentInput with applicationCollection := false }⟩
  , ⟨"schema-mismatch", { validDocumentInput with matchesLive := false }⟩
  , ⟨"no-signer", { validDocumentInput with heldSigner := false }⟩
  , ⟨"non-http", { validDocumentInput with http := false }⟩ ]

def documentCreateCaseJson (c : DocumentCreateCase) : Json := Json.mkObj
  [("name", toJson c.name),
   ("source", toJson (if c.input.source == .authored then "authored" else "remote")),
   ("single_collection", toJson c.input.singleCollection),
   ("application_collection", toJson c.input.applicationCollection),
   ("matches_live", toJson c.input.matchesLive),
   ("held_signer", toJson c.input.heldSigner),
   ("http", toJson c.input.http),
   ("expected_route", toJson (routeString (documentCreateRoute c.input)))]

def documentCreateCasesJson : String := (toJson (documentCreateCases.map documentCreateCaseJson)).compress

end Identity.ClientAgentCreation
