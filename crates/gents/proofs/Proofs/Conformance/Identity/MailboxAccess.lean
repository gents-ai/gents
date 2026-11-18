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

/-- The served readiness fixture admits only client-signed requests; without
the verified admission the client claim queues there, so it observes the
provenance of an unadmitted lifecycle only before a claim commits. -/
def claimedRecipientCases : List ClaimInput :=
  [{ present := false, committed := false }, { present := true, committed := false },
   { present := true, committed := true }]

@[conformance_family]
def claimedRecipientServed : Family ClaimInput RecipientOutput where
  inputs := claimedRecipientCases
  model := recipientOutput

/-- model-production-gap: no served fixture commits a client claim without its
verified admission, so a committed claim with no admitted signer is not built. -/
@[conformance_family]
def claimedRecipientUnreached : Family ClaimInput RecipientOutput where
  inputs := [{ present := false, committed := true }]
  model := recipientOutput

structure ClaimVisibilityInput where
  item : String
  actor : String
  deriving ToJson

/-- A delegated client claim routes both the item its mailbox tool creates and
the claimed request's runtime notice to the immediate signer, so each item
holds the `fixture` recipient relationship with that signer as "user"; an
outsider and the session owner hold no relationship to either and stand as
"other". -/
@[conformance_family]
def claimVisibility : Family ClaimVisibilityInput Bool where
  inputs := [{ item := "tool", actor := "user" }, { item := "tool", actor := "other" },
    { item := "notice", actor := "other" }]
  model := fun i => permitted fixture i.actor "read"

/-- The delegated claim scenario binds its tool context to the admitted client
before and after the claim commits; the context names an admitted client only
once the claim has committed. -/
@[conformance_family]
def claimToolContext : Family ClaimInput Bool where
  inputs := [{ present := true, committed := false }, { present := true, committed := true }]
  model := fun i => (claimedRecipient (admittedFixture i.present) i.committed).isSome

structure NoticeOutput where
  document : Bool
  recipient : Bool
  deferred : Bool
  deriving ToJson

structure NoticeInput where
  present : Bool
  claimed : Bool
  deriving ToJson

@[conformance_family]
def noticePublication : Family NoticeInput NoticeOutput where
  inputs := [{ present := true, claimed := false }, { present := true, claimed := true },
    { present := false, claimed := true }]
  model := fun i =>
    let p := runtimeNotice (admittedFixture i.present) i.claimed true
    { document := p.document, recipient := p.recipient, deferred := !p.document }

end Conformance.MailboxAccess
