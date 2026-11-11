import Proofs.Session.Hooks.Claim
import Proofs.Automation.Mailbox.Reply

namespace Hooks.MailboxPlugin

open Mailbox

def collection : String := "MailboxItem"

def manifest : Manifest := { name := "mailbox", owns := [collection], bound := 1 }

/-- What core hands the plugin at `request.claim.applied`: facts of the
request row the claim just affected. `authenticated` is core's verification
of the request's receipt signature and `signer` its admission signer; the
plugin holds no key material. `now` is the claim's timestamp.

Finding against `session/mailbox/reply.rs`: the deadline is compared with
`chrono::Utc::now()` read inside the transaction closure, not with the claim
timestamp passed to the function. A retried closure can decide differently. A
data hook has no clock, so the deadline is compared with `now`. -/
structure Claimed where
  requestDocId : String
  sourceDocId : Option String
  requesterDid : String
  nodeDid : String
  agentDid : String
  sessionId : String
  interactive : Bool
  authenticated : Bool
  signer : String
  now : Nat
  deriving DecidableEq, Repr

/-- Rows the plugin is shown. `item` is its own document: the stored item,
the reply envelope facts and the deadline. `questionCurrent` in the envelope
is the plugin's check of the request's question-reply input against the item,
so the native event must also carry that input. `access` is core's
observation of document ACP for a subject the plugin named.

Finding: the reply check asks whether the request's signer, not the claiming
runtime, may update the item (`txn.observe_document_access`). The contract
carries this as a declared read answered with a row. It narrows: the write is
still validated against the transaction's actor. -/
inductive Fact where
  | item (item : Item) (envelope : ReplyEnvelope) (deadline : Option Nat)
  | access (allowed : Bool)
  deriving DecidableEq, Repr

inductive Query where
  | item (doc : String)
  | access (subject : String) (doc : String)
  deriving DecidableEq, Repr

def itemRef (doc : String) : Ref String := ⟨collection, doc⟩

def envelopeAt (envelope : ReplyEnvelope) (deadline : Option Nat) (now : Nat) : ReplyEnvelope :=
  { envelope with deadlineValid := deadline.all fun limit => decide (now < limit) }

def evidence (claimed : Claimed) (doc : String) (recipient : Bool) : ReplyEvidence :=
  { requestDocId := claimed.requestDocId
    sourceDocId := doc
    requesterDid := claimed.requesterDid
    nodeDid := claimed.nodeDid
    agentDid := claimed.agentDid
    sessionId := claimed.sessionId
    authenticated := claimed.authenticated
    recipientAuthorized := recipient
    interactive := claimed.interactive }

def replay (claimed : Claimed) (item : Item) : Bool :=
  item.status == .acted && item.resolvedDocId == claimed.requestDocId

/-- The ACP answer is consulted only for an authenticated first claim; an
unavailable answer is not a denial and not a grant. -/
def recipient? (claimed : Claimed) (item : Item) (access : List (Row String Fact)) :
    Option Bool :=
  if claimed.authenticated && !replay claimed item then
    match access with
    | [⟨_, _, .access allowed⟩] => some allowed
    | _ => none
  else some false

/-- The Mailbox reply claim as a plugin policy. The existing owner
`Mailbox.claimReply?` decides; this function only says when it applies and
turns its answer into a refusal or a compare-and-set update of the item.

Finding: `Mailbox.replyMatches` rejects a non-interactive request and an item
that does not take a request reply, while `claim_reply_in_txn` returns without
touching anything for a non-interactive request, a request with no source, a
source that is not a mailbox item, and an item whose action is not
StartRequest. The applicability test below is that Rust behaviour; the
existing model has no definition for it. -/
def decide (claimed : Claimed) (rows : List (List (Row String Fact))) :
    Outcome Unit String Fact Empty :=
  match claimed.interactive, claimed.sourceDocId, rows with
  | true, some doc, [[⟨ref, revision, .item item envelope deadline⟩], access] =>
      if envelope.handling = .startRequest then
        match recipient? claimed item access with
        | none => .refuse "mailbox reply recipient permission is unavailable"
        | some recipient =>
            match claimReply? item (envelopeAt envelope deadline claimed.now)
                (evidence claimed doc recipient) with
            | none => .refuse "mailbox reply rejected"
            | some post =>
                if item.status = .acted then .proceed () []
                else .proceed () [.write (.update ref revision (.item post envelope deadline))]
      else .proceed () []
  | _, _, _ => .proceed () []

def reads (claimed : Claimed) : List Query :=
  match claimed.interactive, claimed.sourceDocId with
  | true, some doc => [.item doc, .access claimed.signer doc]
  | _, _ => []

def plugin : Plugin Query Claimed Unit String Fact Empty :=
  { manifest := manifest, hook := { reads := reads, decide := decide } }

/-- Core's side: item rows by document ID, and the ACP observation for the
named subject rendered as a row. `observe` returning `none` is an unavailable
answer. Mailbox requests no core operation. -/
def host {Core : Type} (observe : String → Ref String → Option Bool) (allowed : Acp String) :
    Host Core Query String Fact Empty :=
  { select := fun _ docs query =>
      match query with
      | .item doc => docs.filter fun row => row.ref == itemRef doc
      | .access subject doc =>
          match observe subject (itemRef doc) with
          | some answer => [⟨⟨"acp", doc⟩, 0, .access answer⟩]
          | none => []
    perform := fun _ op _ => nomatch op
    allowed := allowed }

theorem inapplicable_request_stages_nothing (claimed : Claimed)
    (rows : List (List (Row String Fact)))
    (inapplicable : claimed.interactive = false ∨ claimed.sourceDocId = none) :
    decide claimed rows = .proceed () [] := by
  rcases inapplicable with background | unsourced
  · simp [decide, background]
  · unfold decide
    rw [unsourced]
    cases claimed.interactive <;> rfl

/-- The plugin's answer is the existing owner's answer. -/
theorem decision_is_claimReply (claimed : Claimed) (doc : String) (ref : Ref String)
    (revision : Nat) (item : Item) (envelope : ReplyEnvelope) (deadline : Option Nat)
    (access : List (Row String Fact)) (recipient : Bool)
    (interactive : claimed.interactive = true) (sourced : claimed.sourceDocId = some doc)
    (handling : envelope.handling = .startRequest)
    (observed : recipient? claimed item access = some recipient) :
    decide claimed [[⟨ref, revision, .item item envelope deadline⟩], access] =
      match claimReply? item (envelopeAt envelope deadline claimed.now)
          (evidence claimed doc recipient) with
      | none => .refuse "mailbox reply rejected"
      | some post =>
          if item.status = .acted then .proceed () []
          else .proceed () [.write (.update ref revision (.item post envelope deadline))] := by
  simp [decide, interactive, sourced, handling, observed]

/-- A reply the existing owner admits for an open item stages exactly the
owner's post-state: acted, resolved by the claimed request. -/
theorem open_reply_stages_the_owners_item (claimed : Claimed) (doc : String) (ref : Ref String)
    (revision : Nat) (item post : Item) (envelope : ReplyEnvelope) (deadline : Option Nat)
    (access : List (Row String Fact)) (recipient : Bool)
    (interactive : claimed.interactive = true) (sourced : claimed.sourceDocId = some doc)
    (handling : envelope.handling = .startRequest)
    (observed : recipient? claimed item access = some recipient) (isOpen : item.status = .open)
    (admitted : claimReply? item (envelopeAt envelope deadline claimed.now)
      (evidence claimed doc recipient) = some post) :
    decide claimed [[⟨ref, revision, .item item envelope deadline⟩], access] =
        .proceed () [.write (.update ref revision (.item post envelope deadline))] ∧
      post = { item with status := .acted, resolvedDocId := claimed.requestDocId } := by
  have matched := reply_claim_requires_admission item post _ _ admitted
  have recorded := open_reply_claim_records_receipt item _ _ isOpen matched
  rw [admitted] at recorded
  refine ⟨?_, ?_⟩
  · rw [decision_is_claimReply claimed doc ref revision item envelope deadline access recipient
      interactive sourced handling observed, admitted]
    simp [isOpen]
  · simpa [evidence] using recorded

variable {Core : Type}

/-- Composed with the claim: when the existing owner rejects the reply, the
claim transaction fails with the plugin's refusal and core's pending to
claimed transition is rolled back with it. -/
theorem rejected_reply_rolls_back_the_claim (observe : String → Ref String → Option Bool)
    (allowed : Acp String) (actor : Actor) (core : ClaimCore Core) (claimed : Claimed)
    (state : State Core String Fact) (next : Core) (doc : String) (row : Row String Fact)
    (item : Item) (envelope : ReplyEnvelope) (deadline : Option Nat) (recipient : Bool)
    (wins : core.claim state.core = some next)
    (interactive : claimed.interactive = true) (sourced : claimed.sourceDocId = some doc)
    (stored : state.docs.filter (fun candidate => candidate.ref == itemRef doc) = [row])
    (shape : row.value = .item item envelope deadline)
    (handling : envelope.handling = .startRequest)
    (observed : recipient? claimed item
      ((host (Core := Core) observe allowed).select next state.docs
        (.access claimed.signer doc)) = some recipient)
    (rejected : claimReply? item (envelopeAt envelope deadline claimed.now)
      (evidence claimed doc recipient) = none) :
    claimStep (host observe allowed) [] [plugin] actor (fun _ => ()) (fun _ _ => claimed) core
        state = .error (.refused "mailbox" "mailbox reply rejected") ∧
      settle state (claimStep (host observe allowed) [] [plugin] actor (fun _ => ())
        (fun _ _ => claimed) core state) = state := by
  obtain ⟨ref, revision, value⟩ := row
  simp only at shape
  subst shape
  have decided : plugin.hook.run (host observe allowed) claimed
      ({ state with core := next } : State Core String Fact) =
      .refuse "mailbox reply rejected" := by
    simp only [Hook.run, plugin, reads, interactive, sourced, List.map_cons, List.map_nil]
    have shown : (host (Core := Core) observe allowed).select next state.docs (.item doc) =
        [⟨ref, revision, .item item envelope deadline⟩] := stored
    rw [shown, decision_is_claimReply claimed doc ref revision item envelope deadline _ recipient
      interactive sourced handling observed, rejected]
  have failed : claimStep (host observe allowed) [] [plugin] actor (fun _ => ())
      (fun _ _ => claimed) core state = .error (.refused "mailbox" "mailbox reply rejected") := by
    simp only [claimStep, gate, combine, List.foldl_nil, wins, stage, stageFrom, decided]
    rfl
  exact ⟨failed, by rw [failed]; rfl⟩

/-- Composed with the claim: an admitted reply to an open item commits core's
claim and the owner's acted item together. The item names the claimed request
exactly when that request is claimed; neither exists without the other. -/
theorem admitted_reply_commits_with_the_claim (observe : String → Ref String → Option Bool)
    (allowed : Acp String) (actor : Actor) (core : ClaimCore Core) (claimed : Claimed)
    (state : State Core String Fact) (next : Core) (doc : String) (row : Row String Fact)
    (item post : Item) (envelope : ReplyEnvelope) (deadline : Option Nat) (recipient : Bool)
    (wins : core.claim state.core = some next)
    (interactive : claimed.interactive = true) (sourced : claimed.sourceDocId = some doc)
    (stored : state.docs.filter (fun candidate => candidate.ref == itemRef doc) = [row])
    (shape : row.value = .item item envelope deadline)
    (handling : envelope.handling = .startRequest)
    (observed : recipient? claimed item
      ((host (Core := Core) observe allowed).select next state.docs
        (.access claimed.signer doc)) = some recipient)
    (isOpen : item.status = .open)
    (admitted : claimReply? item (envelopeAt envelope deadline claimed.now)
      (evidence claimed doc recipient) = some post)
    (permitted : allowed actor .update row.ref = true) :
    claimStep (host observe allowed) [] [plugin] actor (fun _ => ()) (fun _ _ => claimed) core
        state =
      .ok ({ core := next
             docs := state.docs.map fun candidate =>
               if held row.ref row.revision candidate then
                 { candidate with
                   revision := row.revision + 1
                   value := .item { item with status := .acted,
                                              resolvedDocId := claimed.requestDocId }
                     envelope deadline }
               else candidate }, .claimed) := by
  obtain ⟨ref, revision, value⟩ := row
  simp only at shape
  subst shape
  have member : (⟨ref, revision, .item item envelope deadline⟩ : Row String Fact) ∈
      state.docs.filter (fun candidate => candidate.ref == itemRef doc) := by
    rw [stored]
    exact List.mem_singleton_self _
  have located := List.mem_filter.mp member
  have named : ref = itemRef doc := by simpa using located.2
  obtain ⟨staged, written⟩ := open_reply_stages_the_owners_item claimed doc ref revision item post
    envelope deadline _ recipient interactive sourced handling observed isOpen admitted
  have decided : plugin.hook.run (host observe allowed) claimed
      ({ state with core := next } : State Core String Fact) =
      .proceed () [.write (.update ref revision (.item post envelope deadline))] := by
    simp only [Hook.run, plugin, reads, interactive, sourced, List.map_cons, List.map_nil]
    have shown : (host (Core := Core) observe allowed).select next state.docs (.item doc) =
        [⟨ref, revision, .item item envelope deadline⟩] := stored
    rw [shown]
    exact staged
  have present : state.docs.any (held ref revision) = true := by
    simp only [List.any_eq_true]
    exact ⟨_, located.1, by simp [held]⟩
  have accepted : valid plugin.manifest (host (Core := Core) observe allowed).allowed actor
      [(.write (.update ref revision (.item post envelope deadline)) :
        Effect String Fact Empty)] = true := by
    have owned : collection = ref.collection := by rw [named]; rfl
    simp [valid, Effect.permitted, plugin, manifest, host, Write.ref, Write.access, owned]
    exact permitted
  simp only [claimStep, gate, combine, List.foldl_nil, wins, stage, stageFrom, decided, accepted,
    if_true, applyEffects, applyEffect, applyWrite, present, Option.map_some']
  rw [written]

end Hooks.MailboxPlugin
