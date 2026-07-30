import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.Prod

namespace PeerRegistryDiscovery
namespace ReciprocalConversation

abbrev Did := String
abbrev PeerId := String
abbrev Address := String

structure BearerReadiness where
  membershipSignatureValid : Bool
  membershipActive : Bool
  acknowledgementSignatureValid : Bool
  reciprocalReplicatorApplied : Bool
  deriving DecidableEq, Repr

def BearerReadiness.ready (r : BearerReadiness) : Bool :=
  r.membershipSignatureValid &&
  r.membershipActive &&
  r.acknowledgementSignatureValid &&
  r.reciprocalReplicatorApplied

theorem bearer_ready_iff_all_verified (r : BearerReadiness) :
    r.ready = true ↔
      r.membershipSignatureValid = true ∧
      r.membershipActive = true ∧
      r.acknowledgementSignatureValid = true ∧
      r.reciprocalReplicatorApplied = true := by
  simp [BearerReadiness.ready, and_assoc]

theorem no_bearer_chat_before_reciprocal_apply (r : BearerReadiness)
    (h : r.reciprocalReplicatorApplied = false) :
    r.ready = false := by
  simp [BearerReadiness.ready, h]

theorem no_bearer_chat_before_active_membership (r : BearerReadiness)
    (h : r.membershipActive = false) :
    r.ready = false := by
  simp [BearerReadiness.ready, h]

structure ReciprocalIntent where
  memberDid : Did
  template : String
  deriving DecidableEq, Repr

structure PeerEndpoint where
  did : Did
  peer : PeerId
  address : Address
  live : Bool
  deriving DecidableEq, Repr

structure DataPlaneRow where
  peer : PeerId
  agentDid : Did
  address : Address
  template : String
  deriving DecidableEq, Repr

def materializable (intent : ReciprocalIntent) (endpoint : PeerEndpoint) : Prop :=
  intent.template = "conversation" ∧
  endpoint.live = true ∧
  endpoint.did = intent.memberDid ∧
  endpoint.peer ≠ "" ∧
  endpoint.address ≠ ""

instance (intent : ReciprocalIntent) (endpoint : PeerEndpoint) :
    Decidable (materializable intent endpoint) := by
  unfold materializable
  infer_instance

def reciprocalDataPlaneDesired
    (self : Did)
    (intent : ReciprocalIntent)
    (endpoint : PeerEndpoint) : Option DataPlaneRow :=
  if materializable intent endpoint then
    some {
      peer := endpoint.peer,
      agentDid := self,
      address := endpoint.address,
      template := "conversation"
    }
  else
    none

def deriveReciprocal
    (self : Did)
    (intents : Finset ReciprocalIntent)
    (endpoints : Finset PeerEndpoint)
    (revokedMembers : Finset Did) : Finset DataPlaneRow :=
  (((intents ×ˢ endpoints).filter (fun pair =>
      pair.1.memberDid ∉ revokedMembers ∧ materializable pair.1 pair.2))).image
    (fun pair => {
      peer := pair.2.peer,
      agentDid := self,
      address := pair.2.address,
      template := "conversation"
    })

structure ReciprocalState where
  self : Did
  intents : Finset ReciprocalIntent
  endpoints : Finset PeerEndpoint
  revokedMembers : Finset Did
  operatorDesired : Finset DataPlaneRow
  networkDesired : Finset DataPlaneRow
  registryDesired : Finset DataPlaneRow
  reciprocalDesired : Finset DataPlaneRow
  deriving DecidableEq

namespace ReciprocalState

def settled (s : ReciprocalState) : Prop :=
  s.reciprocalDesired = deriveReciprocal s.self s.intents s.endpoints s.revokedMembers

instance (s : ReciprocalState) : Decidable s.settled := by
  unfold settled
  infer_instance

def deriveStep (s : ReciprocalState) : ReciprocalState :=
  { s with reciprocalDesired :=
      deriveReciprocal s.self s.intents s.endpoints s.revokedMembers }

end ReciprocalState

open ReciprocalState

theorem mem_deriveReciprocal {self : Did}
    {intents : Finset ReciprocalIntent}
    {endpoints : Finset PeerEndpoint}
    {revokedMembers : Finset Did}
    {row : DataPlaneRow} :
    row ∈ deriveReciprocal self intents endpoints revokedMembers ↔
      ∃ intent ∈ intents, ∃ endpoint ∈ endpoints,
        intent.memberDid ∉ revokedMembers ∧ materializable intent endpoint ∧
        row = {
          peer := endpoint.peer,
          agentDid := self,
          address := endpoint.address,
          template := "conversation"
        } := by
  unfold deriveReciprocal
  simp only [Finset.mem_image, Finset.mem_filter, Finset.mem_product]
  constructor
  · rintro ⟨pair, ⟨⟨h_intent, h_endpoint⟩, h_revoked, h_materializable⟩, h_row⟩
    exact ⟨pair.1, h_intent, pair.2, h_endpoint,
      h_revoked, h_materializable, h_row.symm⟩
  · rintro ⟨intent, h_intent, endpoint, h_endpoint,
      h_revoked, h_materializable, h_row⟩
    exact ⟨(intent, endpoint),
      ⟨⟨h_intent, h_endpoint⟩, h_revoked, h_materializable⟩, h_row.symm⟩

theorem deriveReciprocal_idempotent (s : ReciprocalState) :
    deriveStep (deriveStep s) = deriveStep s := by
  unfold deriveStep
  rfl

theorem deriveReciprocal_settles (s : ReciprocalState) :
    (deriveStep s).settled := by
  unfold settled deriveStep
  rfl

theorem deriveReciprocal_convergent {s : ReciprocalState} (h : s.settled) :
    deriveStep s = s := by
  unfold settled at h
  unfold deriveStep
  cases s
  simp only at h ⊢
  subst h
  rfl

theorem deriveReciprocal_ownership_safe (s : ReciprocalState) :
    (deriveStep s).operatorDesired = s.operatorDesired ∧
    (deriveStep s).networkDesired = s.networkDesired ∧
    (deriveStep s).registryDesired = s.registryDesired := by
  exact ⟨rfl, rfl, rfl⟩

theorem deriveReciprocal_retraction_sound_intent {self : Did}
    {intents : Finset ReciprocalIntent}
    {endpoints : Finset PeerEndpoint}
    {revokedMembers : Finset Did}
    {removed : ReciprocalIntent}
    {row : DataPlaneRow} :
    row ∈ deriveReciprocal self (intents.erase removed) endpoints revokedMembers ↔
      ∃ intent ∈ intents, intent ≠ removed ∧ ∃ endpoint ∈ endpoints,
        intent.memberDid ∉ revokedMembers ∧ materializable intent endpoint ∧
        row = {
          peer := endpoint.peer,
          agentDid := self,
          address := endpoint.address,
          template := "conversation"
        } := by
  rw [mem_deriveReciprocal]
  constructor
  · rintro ⟨intent, h_intent, endpoint, h_endpoint,
      h_revoked, h_materializable, h_row⟩
    exact ⟨intent, Finset.mem_of_mem_erase h_intent,
      Finset.ne_of_mem_erase h_intent, endpoint, h_endpoint,
      h_revoked, h_materializable, h_row⟩
  · rintro ⟨intent, h_intent, h_ne, endpoint, h_endpoint,
      h_revoked, h_materializable, h_row⟩
    exact ⟨intent, Finset.mem_erase.mpr ⟨h_ne, h_intent⟩,
      endpoint, h_endpoint, h_revoked, h_materializable, h_row⟩

theorem deriveReciprocal_retraction_sound_endpoint {self : Did}
    {intents : Finset ReciprocalIntent}
    {endpoints : Finset PeerEndpoint}
    {revokedMembers : Finset Did}
    {removed : PeerEndpoint}
    {row : DataPlaneRow} :
    row ∈ deriveReciprocal self intents (endpoints.erase removed) revokedMembers ↔
      ∃ intent ∈ intents, ∃ endpoint ∈ endpoints, endpoint ≠ removed ∧
        intent.memberDid ∉ revokedMembers ∧ materializable intent endpoint ∧
        row = {
          peer := endpoint.peer,
          agentDid := self,
          address := endpoint.address,
          template := "conversation"
        } := by
  rw [mem_deriveReciprocal]
  constructor
  · rintro ⟨intent, h_intent, endpoint, h_endpoint,
      h_revoked, h_materializable, h_row⟩
    exact ⟨intent, h_intent, endpoint, Finset.mem_of_mem_erase h_endpoint,
      Finset.ne_of_mem_erase h_endpoint, h_revoked, h_materializable, h_row⟩
  · rintro ⟨intent, h_intent, endpoint, h_endpoint, h_ne,
      h_revoked, h_materializable, h_row⟩
    exact ⟨intent, h_intent, endpoint, Finset.mem_erase.mpr ⟨h_ne, h_endpoint⟩,
      h_revoked, h_materializable, h_row⟩

theorem deriveReciprocal_retraction_sound_revocation {self : Did}
    {intents : Finset ReciprocalIntent}
    {endpoints : Finset PeerEndpoint}
    {revokedMembers : Finset Did}
    {revokedDid : Did}
    {row : DataPlaneRow} :
    row ∈ deriveReciprocal self intents endpoints (insert revokedDid revokedMembers) ↔
      ∃ intent ∈ intents, intent.memberDid ≠ revokedDid ∧
        intent.memberDid ∉ revokedMembers ∧ ∃ endpoint ∈ endpoints,
        materializable intent endpoint ∧
        row = {
          peer := endpoint.peer,
          agentDid := self,
          address := endpoint.address,
          template := "conversation"
        } := by
  rw [mem_deriveReciprocal]
  constructor
  · rintro ⟨intent, h_intent, endpoint, h_endpoint,
      h_not_revoked, h_materializable, h_row⟩
    have h_parts : intent.memberDid ≠ revokedDid ∧
        intent.memberDid ∉ revokedMembers := by
      simpa using h_not_revoked
    exact ⟨intent, h_intent, h_parts.1, h_parts.2,
      endpoint, h_endpoint, h_materializable, h_row⟩
  · rintro ⟨intent, h_intent, h_ne, h_not_revoked,
      endpoint, h_endpoint, h_materializable, h_row⟩
    have h_not_insert : intent.memberDid ∉ insert revokedDid revokedMembers := by
      simpa using And.intro h_ne h_not_revoked
    exact ⟨intent, h_intent, endpoint, h_endpoint,
      h_not_insert, h_materializable, h_row⟩

theorem stale_endpoint_not_materializable (intent : ReciprocalIntent)
    (endpoint : PeerEndpoint) :
    ¬ materializable intent { endpoint with live := false } := by
  unfold materializable
  intro h
  exact Bool.noConfusion h.2.1

end ReciprocalConversation
end PeerRegistryDiscovery
