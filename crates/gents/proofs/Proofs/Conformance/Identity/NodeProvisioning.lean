import Proofs.Store.Identity.NodeProvisioning
import Proofs.Store.Identity.NodeProvisioningConformance
import Proofs.Conformance.Family
import Proofs.Tools.SelfConfig.CategoryGrants

namespace Conformance.NodeProvisioning

open Lean Identity Identity.NodeProvisioning

structure ExposureInput where
  name : String
  nodeCategory : Bool
  agentCategory : Bool
  deriving ToJson

@[conformance_family]
def agentCreationExposure : Family ExposureInput Bool where
  inputs :=
    [("agent-category", false, true), ("node-category", true, false),
      ("both-categories", true, true), ("no-category", false, false)].map
      fun (name, node, agent) =>
        ({ name, nodeCategory := node, agentCategory := agent } : ExposureInput)
  model := fun i => agentCommandExposed i.nodeCategory i.agentCategory

structure ReservationInput where
  name : String
  created : Bool
  fileSynced : Bool
  directorySynced : Bool
  deriving ToJson

def reservationInputs : List ReservationInput :=
  [("confirmed", true, true, true), ("exists-or-create-failed", false, false, false),
    ("file-sync-failed", true, false, false), ("directory-sync-failed", true, true, false)].map
    fun (name, created, fileSynced, directorySynced) =>
      { name, created, fileSynced, directorySynced }

/-- A home can already hold the marker, and a home that cannot be opened for its
directory sync fails that step after the marker is written; nothing makes a
created marker's own file sync fail. -/
@[conformance_family]
def reservation : Family ReservationInput Bool where
  inputs := reservationInputs.filter fun i => !i.created || i.fileSynced
  model := fun i => reservationConfirmed i.created i.fileSynced i.directorySynced

/-- model-production-gap: no production path makes the file sync of a newly
created bootstrap marker fail. -/
@[conformance_family]
def reservationUnreached : Family ReservationInput Bool where
  inputs := reservationInputs.filter fun i => i.created && !i.fileSynced
  model := fun i => reservationConfirmed i.created i.fileSynced i.directorySynced

structure BootstrapInput where
  name : String
  exclusive : Bool
  matchingRuntime : Bool
  authenticatedClient : Bool
  nodeAbsentObserved : Bool
  storeEmptyObserved : Bool
  reservationDurable : Bool
  nacEnabled : Bool
  freshEvidence : Bool
  configurationPublished : Bool
  deriving ToJson

def BootstrapInput.ofModel (name : String) (o : BootstrapObservation) : BootstrapInput :=
  { name, exclusive := o.exclusive, matchingRuntime := o.matchingRuntime,
    authenticatedClient := o.authenticatedClient, nodeAbsentObserved := o.nodeAbsentObserved,
    storeEmptyObserved := o.storeEmptyObserved, reservationDurable := o.reservationDurable,
    nacEnabled := o.nacEnabled, freshEvidence := o.freshEvidence,
    configurationPublished := o.configurationPublished }

def BootstrapInput.toModel (i : BootstrapInput) : BootstrapObservation :=
  ⟨i.exclusive, i.matchingRuntime, i.authenticatedClient, i.nodeAbsentObserved,
    i.storeEmptyObserved, i.reservationDurable, i.nacEnabled, i.freshEvidence,
    i.configurationPublished⟩

/-- Init builds a fresh home, and a home whose marker survived a crash before
its grants; each other observation is a separate refusal inside init that no
test home can be put into without replacing init's own steps. -/
def bootstrapReached : List String := ["fresh", "crash-after-reservation"]

@[conformance_family]
def bootstrapGrant : Family BootstrapInput Bool where
  inputs := (Identity.NodeProvisioningConformance.bootstrapCases.filter
    fun (name, _) => name ∈ bootstrapReached).map fun (name, o) => BootstrapInput.ofModel name o
  model := fun i => bootstrapMayGrant i.toModel

/-- model-production-gap: init decides these inline across its store lock,
runtime match, client authentication, Node and store observation, NAC status and
publication steps; no production function takes the observation. -/
@[conformance_family]
def bootstrapGrantUnreached : Family BootstrapInput Bool where
  inputs := (Identity.NodeProvisioningConformance.bootstrapCases.filter
    fun (name, _) => name ∉ bootstrapReached).map fun (name, o) => BootstrapInput.ofModel name o
  model := fun i => bootstrapMayGrant i.toModel

deriving instance ToJson for NacStatus

structure NacInput where
  status : NacStatus
  deriving ToJson

structure NacOutput where
  mayStart : Bool
  deriving ToJson

private def nacModel (i : NacInput) : NacOutput := { mayStart := runtimeMayStart i.status }

/-- A runtime start opens its node with NAC enabled, and an admin can disable
it on the running node, which native persists across a restart: these are the
statuses a production start reaches. -/
@[conformance_family]
def runtimeNac : Family NacInput NacOutput where
  inputs := [{ status := .enabled }, { status := .disabledTemporarily }]
  model := nacModel

/-- model-production-gap: every production node open enables NAC, so no
production path observes a node whose NAC was never configured. -/
@[conformance_family]
def runtimeNacUnreached : Family NacInput NacOutput where
  inputs := [{ status := .notConfigured }]
  model := nacModel

structure CreatorInput where
  name : String
  runtimeMatches : Bool
  signerHeld : Bool
  signerMatches : Bool
  nativeCreate : Bool
  nativeRelation : Bool
  deriving ToJson

def CreatorInput.observation (i : CreatorInput) : AgentCreatorObservation :=
  ⟨i.runtimeMatches, i.signerHeld, i.signerMatches, i.nativeCreate, i.nativeRelation⟩

private def creatorInput (c : String × AgentCreatorObservation) : CreatorInput :=
  { name := c.1, runtimeMatches := c.2.runtimeMatches, signerHeld := c.2.signerHeld
    signerMatches := c.2.signerMatches, nativeCreate := c.2.nativeCreate
    nativeRelation := c.2.nativeRelation }

/-- The served creation scenario holds a node whose native document ACP admits
the creator, so it can vary the runtime and the signer but not the native
create and relation decisions. -/
private def nativeAdmits (c : String × AgentCreatorObservation) : Bool :=
  c.2.nativeCreate && c.2.nativeRelation

@[conformance_family]
def agentCreator : Family CreatorInput Bool where
  inputs := (NodeProvisioningConformance.agentCreatorCases.filter nativeAdmits).map creatorInput
  model := fun i => agentCreatorMayPublish i.observation

/-- model-production-gap: no served creation scenario denies the native Agent
create or its parent relation to an otherwise authorized creator yet. -/
@[conformance_family]
def agentCreatorUnreached : Family CreatorInput Bool where
  inputs :=
    (NodeProvisioningConformance.agentCreatorCases.filter (!nativeAdmits ·)).map creatorInput
  model := fun i => agentCreatorMayPublish i.observation

/-- A User-created Engineer on a node the User owns, selected as its default
Agent, before `SelfConfig.provisionEngineer`. -/
def unprovisionedEngineer : Identity.Household.State :=
  SelfConfig.initDefaultSelection
    (Identity.Household.commitAgentCreation
      { objects := [⟨"nodes", "node", "user"⟩], relations := [], enrolled := [] }
      "engineer" "user" "engineer" "node" true) "node" "user"

structure EngineerGrantInput where
  provisioned : Bool
  resource : String
  object : String
  permission : String
  deriving ToJson

/-- What the Engineer may do on its Node, its own Agent and the default
selection, before and after provisioning: the agent catalog and update of
every Agent come only from provisioning. -/
@[conformance_family]
def engineerGrants : Family EngineerGrantInput Bool where
  inputs := [false, true].flatMap fun provisioned =>
    [("nodes", "node", "read"), ("nodes", "node", "update"), ("nodes", "node", "manage_agents"),
      ("agents", "engineer", "read"), ("agents", "engineer", "update"),
      ("agents", "engineer", "delete"), ("default_agent_selections", "node", "read"),
      ("default_agent_selections", "node", "update")].map
      fun (resource, object, permission) => { provisioned, resource, object, permission }
  model := fun i =>
    let state := if i.provisioned then
      SelfConfig.provisionEngineer unprovisionedEngineer "engineer" "node"
    else unprovisionedEngineer
    Identity.Household.allowed 8 state "engineer" i.resource i.object i.permission

end Conformance.NodeProvisioning
