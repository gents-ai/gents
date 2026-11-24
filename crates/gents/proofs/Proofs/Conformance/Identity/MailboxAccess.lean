import Proofs.Store.Identity.MailboxAccess
import Proofs.Conformance.Family

namespace Conformance.MailboxAccess

open Lean Identity.MailboxAccess

structure SweepActorInput where
  actor : String
  deriving ToJson

@[conformance_family]
def sweepActors : Family SweepActorInput Bool where
  inputs := ["agent", "other", "node"].map fun actor => { actor }
  model := fun i => sweepVisible i.actor "agent"

structure NativeInput where
  actors : List String
  permissions : List String
  deriving ToJson

structure NativeCheck where
  actor : String
  permission : String
  expected : Bool
  revokedExpected : Bool
  deriving ToJson

structure NativeOutput where
  policyYaml : String
  abortedDocument : Bool
  checks : List NativeCheck
  deriving ToJson

@[conformance_family]
def native : Family NativeInput NativeOutput where
  inputs :=
    [{ actors := ["agent", "user", "other", "runtime"]
       permissions := ["read", "update", "delete"] }]
  model := fun i =>
    { policyYaml := Identity.Household.policyYamlOf candidatePolicy
      abortedDocument := (publish true true false).document
      checks := i.actors.flatMap fun actor => i.permissions.map fun permission =>
        { actor := actor, permission := permission,
          expected := permitted fixture actor permission,
          revokedExpected := permitted revoked actor permission } }

structure ClaimInput where
  present : Bool
  committed : Bool
  deriving ToJson

def admittedFixture (present : Bool) : Option AdmittedRequest :=
  if present then some ⟨"delegate", "request", "session", "agent"⟩ else none

structure RecipientOutput where
  signer : Option String
  requestDoc : Option String
  sessionDoc : Option String
  agentDoc : Option String
  deriving ToJson

def recipientOutput (i : ClaimInput) : RecipientOutput :=
  let result := claimedRecipient (admittedFixture i.present) i.committed
  { signer := result.map (·.signer)
    requestDoc := result.map (·.requestDoc)
    sessionDoc := result.map (·.sessionDoc)
    agentDoc := result.map (·.agentDoc) }

/-- The readiness fixture observes a client-signed request before and after
its admitted claim commits, the same request through a lifecycle that holds no
verified admission, and a local-self request the managed Agent signs, which
commits its claim without a client admission. -/
@[conformance_family]
def claimedRecipientProvenance : Family ClaimInput RecipientOutput where
  inputs :=
    [{ present := false, committed := false }, { present := false, committed := true },
     { present := true, committed := false }, { present := true, committed := true }]
  model := recipientOutput

structure ClaimVisibilityInput where
  item : String
  actor : String
  deriving ToJson

/-- A delegated client claim routes the item its mailbox tool creates to the
immediate signer, so the item holds the `fixture` recipient relationship with
that signer as "user"; an outsider holds no relationship to it and stands as
"other". -/
@[conformance_family]
def claimVisibility : Family ClaimVisibilityInput Bool where
  inputs := [{ item := "tool", actor := "user" }, { item := "tool", actor := "other" }]
  model := fun i => permitted fixture i.actor "read"

/-- The delegated claim scenario binds its tool context to the admitted client
before and after the claim commits; the context names an admitted client only
once the claim has committed. -/
@[conformance_family]
def claimToolContext : Family ClaimInput Bool where
  inputs := [{ present := true, committed := false }, { present := true, committed := true }]
  model := fun i => (claimedRecipient (admittedFixture i.present) i.committed).isSome

deriving instance ToJson for Admission, Hop

structure FilingInput where
  claimedSigner : Option String
  lineage : List Hop
  nodeOwner : Option String
  deriving ToJson

/-- A claimed request; continuations of a client-signed request, one and two
turns deep; a trigger's run and its continuation, with the Node's owner
observed or not; and the requests that file for no one: an unclaimed signed
request, a peer's, a continuation whose source is not its parent, and an empty
lineage. -/
@[conformance_family]
def filing : Family FilingInput (Option String) where
  inputs :=
    [{ claimedSigner := some "user", lineage := [⟨.client, "user"⟩], nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.localControl, "agent"⟩, ⟨.client, "user"⟩],
       nodeOwner := some "owner" },
     { claimedSigner := none,
       lineage := [⟨.localControl, "agent"⟩, ⟨.localControl, "agent"⟩, ⟨.client, "user"⟩],
       nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.automatedTrigger, "agent"⟩],
       nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.localControl, "agent"⟩, ⟨.automatedTrigger, "agent"⟩],
       nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.automatedTrigger, "agent"⟩], nodeOwner := none },
     { claimedSigner := none, lineage := [⟨.client, "user"⟩], nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.other, "peer"⟩], nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.localControl, "agent"⟩, ⟨.other, "user"⟩],
       nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [⟨.localControl, "agent"⟩], nodeOwner := some "owner" },
     { claimedSigner := none, lineage := [], nodeOwner := some "owner" }]
  model := fun i => filingRecipient i.claimedSigner i.lineage i.nodeOwner

end Conformance.MailboxAccess
