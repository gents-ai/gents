/-!
# Operations on configuration documents: one decision for every client

A Skill and an MCP service are saved, imported, enabled, disabled and deleted
by the same handlers whoever asks: the CLI, the desktop, the shell or an
Agent's config tool. The decisions below read the request, what is stored and
who acts; they read nothing about the client.

Decisions (maintainer, 2026-11-11, #3654):

* An edit is a patch. A field the request omits keeps its stored value.
* A delete is scoped to one owner. A call that names no owner reaches a
  document only when exactly one owner holds that ID.
* Replacing a stored Skill by import takes an explicit overwrite, for every
  actor.
* What differs by actor is stated as a rule about the actor: an Agent imports
  only from inside its tool root, and deletes only with the digest a preview
  returned; a person at a terminal reads any path the process can and deletes
  in one call.
* Registering an MCP service again changes its endpoint and nothing else: a
  disabled service stays disabled and its `send_node_did` choice stays.

Rust owner: `crates/gents-documents/src/operations.rs` and its modules.
-/

namespace DocumentOperations

/-- Who acts. A person signs as the User and may do what an operator may; an
Agent signs as itself. -/
inductive Actor where
  | user
  | agent
  deriving DecidableEq, Repr

/-- One field of a patch. -/
inductive FieldUpdate where
  | omitted
  | clear
  | set
  deriving DecidableEq, Repr

inductive FieldAfter where
  | absent
  | stored
  | requested
  deriving DecidableEq, Repr

/-- An edit is a patch: only a field the request names changes. -/
def fieldAfter (update : FieldUpdate) (storedPresent : Bool) : FieldAfter :=
  match update with
  | .omitted => if storedPresent then .stored else .absent
  | .clear => .absent
  | .set => .requested

theorem omitted_field_keeps_its_stored_value :
    fieldAfter .omitted true = .stored := rfl

theorem only_a_named_field_changes (update : FieldUpdate) (storedPresent : Bool) :
    fieldAfter update storedPresent ≠ .stored ∨ update = .omitted := by
  cases update <;> cases storedPresent <;> decide

inductive WriteMode where
  | create
  | update
  | save
  deriving DecidableEq, Repr

inductive WriteOutcome where
  | created
  | updated
  | refusedExists
  | refusedMissing
  deriving DecidableEq, Repr

/-- `create` refuses a stored document and `update` a missing one; `save` is
the form a client that holds a whole document sends. -/
def writeOutcome (mode : WriteMode) (stored : Bool) : WriteOutcome :=
  match mode, stored with
  | .create, true => .refusedExists
  | .create, false => .created
  | .update, true => .updated
  | .update, false => .refusedMissing
  | .save, true => .updated
  | .save, false => .created

theorem create_never_replaces (stored : Bool) :
    writeOutcome .create stored ≠ .updated := by
  cases stored <;> decide

/-- How many owners hold a document with the requested ID, as the acting
identity can read them. -/
inductive OwnerCount where
  | none
  | one
  | several
  deriving DecidableEq, Repr

inductive OwnerResolution where
  | named
  | sole
  | refusedMissing
  | refusedAmbiguous
  deriving DecidableEq, Repr

/-- The owner an enable, disable or delete acts on. It is one owner or a
refusal; no call reaches the same ID under several owners. -/
def ownerResolution (ownerNamed : Bool) (owners : OwnerCount) : OwnerResolution :=
  if ownerNamed then .named
  else match owners with
    | .none => .refusedMissing
    | .one => .sole
    | .several => .refusedAmbiguous

theorem several_owners_are_never_reached_unnamed :
    ownerResolution false .several = .refusedAmbiguous := rfl

/-- What a delete call carries: nothing, or the digest a preview returned,
which either still matches the stored content or no longer does. -/
inductive Confirmation where
  | absent
  | current
  | stale
  deriving DecidableEq, Repr

inductive DeleteOutcome where
  | deleted
  | refusedNeedsPreview
  | refusedMissing
  | refusedChanged
  | refusedReferenced
  deriving DecidableEq, Repr

/-- One delete for every client. An Agent deletes only what it previewed; a
digest anyone sends is checked; a document another document still selects is
kept. -/
def deleteOutcome (actor : Actor) (confirmation : Confirmation)
    (stored referenced : Bool) : DeleteOutcome :=
  if actor = .agent ∧ confirmation = .absent then .refusedNeedsPreview
  else if !stored then .refusedMissing
  else if confirmation = .stale then .refusedChanged
  else if referenced then .refusedReferenced
  else .deleted

theorem agent_deletes_only_what_it_previewed (confirmation : Confirmation)
    (stored referenced : Bool) :
    deleteOutcome .agent confirmation stored referenced = .deleted →
      confirmation = .current := by
  cases confirmation <;> cases stored <;> cases referenced <;> decide

theorem user_deletes_in_one_call :
    deleteOutcome .user .absent true false = .deleted := rfl

theorem delete_keeps_a_selected_document (actor : Actor) (confirmation : Confirmation)
    (stored : Bool) :
    deleteOutcome actor confirmation stored true ≠ .deleted := by
  cases actor <;> cases confirmation <;> cases stored <;> decide

theorem delete_checks_any_digest_sent (actor : Actor) (stored referenced : Bool) :
    deleteOutcome actor .stale stored referenced ≠ .deleted := by
  cases actor <;> cases stored <;> cases referenced <;> decide

inductive ImportOutcome where
  | created
  | replaced
  | refusedOutsideRoot
  | refusedExists
  deriving DecidableEq, Repr

/-- Importing one skill source. Confinement to the tool root binds an Agent
and no one else; replacing a stored Skill takes the overwrite from everyone. -/
def importOutcome (actor : Actor) (insideToolRoot stored overwrite : Bool) :
    ImportOutcome :=
  if actor = .agent ∧ insideToolRoot = false then .refusedOutsideRoot
  else if stored ∧ overwrite = false then .refusedExists
  else if stored then .replaced
  else .created

theorem import_replaces_only_with_overwrite (actor : Actor)
    (insideToolRoot stored overwrite : Bool) :
    importOutcome actor insideToolRoot stored overwrite = .replaced →
      overwrite = true := by
  cases actor <;> cases insideToolRoot <;> cases stored <;> cases overwrite <;> decide

theorem agent_imports_only_inside_its_root (stored overwrite : Bool) :
    importOutcome .agent false stored overwrite = .refusedOutsideRoot := by
  cases stored <;> cases overwrite <;> decide

theorem user_import_is_not_path_bound (insideToolRoot stored overwrite : Bool) :
    importOutcome .user insideToolRoot stored overwrite ≠ .refusedOutsideRoot := by
  cases insideToolRoot <;> cases stored <;> cases overwrite <;> decide

/-! ## MCP service registration -/

inductive EndpointScheme where
  | http
  | https
  deriving DecidableEq, Repr

inductive EndpointHost where
  | ipv4
  | name
  deriving DecidableEq, Repr

inductive EndpointStorage where
  | verbatimUrl
  | lanIp
  | hostname
  deriving DecidableEq, Repr

/-- Where a registered endpoint is stored. HTTPS names its host in TLS and is
kept verbatim; plain HTTP is kept as address fields, so the pool can choose
between a LAN and a tailnet address. -/
def endpointStorage (scheme : EndpointScheme) (host : EndpointHost) : EndpointStorage :=
  match scheme, host with
  | .https, _ => .verbatimUrl
  | .http, .ipv4 => .lanIp
  | .http, .name => .hostname

/-- A plain-HTTP endpoint without a path is the conventional `/mcp`. -/
def defaultsPath (scheme : EndpointScheme) (pathIsRoot : Bool) : Bool :=
  scheme = .http && pathIsRoot

inductive Toggle where
  | omitted
  | on
  | off
  deriving DecidableEq, Repr

inductive StoredToggle where
  | absent
  | on
  | off
  deriving DecidableEq, Repr

/-- `enabled` after a registration. A new service starts enabled; registering
a stored one again leaves the operator's choice alone unless the call names
one. -/
def enabledAfterRegister (stored : StoredToggle) (requested : Toggle) : Bool :=
  match requested, stored with
  | .on, _ => true
  | .off, _ => false
  | .omitted, .off => false
  | .omitted, _ => true

/-- `send_node_did` after a registration. A new service does not receive the
Node's DID; a stored choice stays unless the call names one. -/
def sendNodeDidAfterRegister (stored : StoredToggle) (requested : Toggle) : Bool :=
  match requested, stored with
  | .on, _ => true
  | .off, _ => false
  | .omitted, .on => true
  | .omitted, _ => false

theorem registering_again_keeps_a_service_disabled :
    enabledAfterRegister .off .omitted = false := rfl

theorem registering_again_keeps_send_node_did :
    sendNodeDidAfterRegister .on .omitted = true := rfl

end DocumentOperations
