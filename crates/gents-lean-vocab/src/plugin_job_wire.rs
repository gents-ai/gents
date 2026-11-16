//! Wire shapes of the Lean plugin-job model (`Proofs.Conformance.PluginJob`)
//! that plugin-job families carry as JSON fields and their consumers decode.
//! The decoder generator emits only types a snapshot key reaches, and no key
//! reaches these.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCall {
    pub argv: Vec<String>,
    pub args: Option<String>,
    pub script: Option<String>,
    pub agent_budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCeiling {
    pub targets: Vec<String>,
    pub max_live: u32,
    pub max_spawns: u32,
    pub max_messages_per_slot: u32,
    pub result_bytes: u64,
    pub max_wall_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobChild {
    pub retry_key: String,
    pub slot: String,
    pub attempt: u32,
    pub target: LeanPluginJobTarget,
    pub state: String,
    pub messages: u32,
}

/// The host facts one step reads, tabulated over the documents and working
/// directories the case names.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobContext {
    pub live: LeanPluginJobCeiling,
    pub bindings: Vec<LeanPluginJobWireBinding>,
    pub available: bool,
    pub caller_targets: LeanPluginJobScope,
    pub job_targets: LeanPluginJobScope,
    pub policy: LeanPluginJobPolicy,
    pub engine_digest: String,
    pub engine_provenance_operator: bool,
    pub max_hop: Option<u32>,
    pub target_hops: Vec<LeanPluginJobTargetHop>,
    pub live_background: u32,
    pub started_by_job: bool,
    pub cwd_allowed: Vec<String>,
    pub within: Vec<LeanPluginJobWithin>,
    #[serde(default)]
    pub script_digest: Option<String>,
    pub adoption_facts: LeanPluginJobHostFacts,
    pub caller_scope: LeanPluginJobProcessScope,
    pub control_targets: Vec<LeanPluginJobControlTarget>,
    pub resume_targets: Vec<LeanPluginJobResumeTarget>,
    pub resume_facts: LeanPluginJobHostFacts,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControl {
    pub row_doc_id: String,
    pub control: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControlOwner {
    pub owner: LeanPluginJobProcessScope,
    pub same_plugin: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControlTarget {
    pub row_doc_id: String,
    pub target: Option<LeanPluginJobControlOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobExtras {
    pub writes: u32,
    pub scratch: Vec<String>,
    pub progress: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobGuestPin {
    pub script_id: String,
    pub source_digest: String,
    pub handle: String,
    pub agent_budget: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobHostFacts {
    pub engine_digest: String,
    pub plugin_ref: String,
    pub cwd: String,
    pub args_digest: String,
    pub chain_root: String,
    pub origin: String,
    pub adopted_at: u64,
    pub resume_of: Option<String>,
    pub replayed_from: Option<String>,
    pub ceiling_at_pin: LeanPluginJobCeiling,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKill {
    pub retry_key: String,
    pub action: String,
}

/// A step output, tagged by `kind`; the sibling riders follow the variant.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginJobOutput {
    Result {
        extras: LeanPluginJobExtras,
        control: Vec<LeanPluginJobControl>,
        pin: Option<LeanPluginJobGuestPin>,
        resume: Option<LeanPluginJobResume>,
    },
    Fail {
        extras: LeanPluginJobExtras,
    },
    Suspend {
        suspend: String,
        reason: Option<String>,
        extras: LeanPluginJobExtras,
        pin: Option<LeanPluginJobGuestPin>,
        resume: Option<LeanPluginJobResume>,
    },
    Effects {
        pin: Option<LeanPluginJobGuestPin>,
        resume: Option<LeanPluginJobResume>,
        spawns: Vec<LeanPluginJobSpawn>,
        messages: Vec<String>,
        interrupts: Vec<String>,
        extras: LeanPluginJobExtras,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPin {
    pub script_id: String,
    pub source_digest: String,
    pub engine_digest: String,
    pub plugin_ref: String,
    pub handle: String,
    pub cwd: String,
    pub args_digest: String,
    pub chain_root: String,
    pub origin: String,
    pub agent_budget: u32,
    pub adopted_at: u64,
    pub resume_of: Option<String>,
    pub replayed_from: Option<String>,
    pub ceiling_at_pin: LeanPluginJobCeiling,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPolicy {
    pub mode: String,
    pub dynamic_targets: Vec<String>,
    pub trusted_packs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobProcessScope {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResume {
    pub row_doc_id: String,
    pub engine_current: bool,
    pub answer: Option<String>,
    pub budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResumeOwner {
    pub owner: LeanPluginJobProcessScope,
    pub same_plugin: bool,
    pub prior: LeanPluginJobRow,
    pub root: LeanPluginJobRow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResumeTarget {
    pub row_doc_id: String,
    pub is_head: bool,
    pub target: Option<LeanPluginJobResumeOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRootCall {
    pub argv: Vec<String>,
    pub args: Option<String>,
    pub agent_budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRow {
    pub root: String,
    pub row_doc_id: String,
    pub caller_session: String,
    pub caller_cwd: String,
    pub handle: String,
    pub caller_hop: u32,
    pub call: LeanPluginJobCall,
    pub root_call: Option<LeanPluginJobRootCall>,
    pub resume: Option<LeanPluginJobResume>,
    pub pin: Option<LeanPluginJobPin>,
    pub children: Vec<LeanPluginJobChild>,
    pub spent: u32,
    pub observed_terminal: u32,
    pub consecutive_failures: u32,
    pub state: String,
}

/// `kind` is `none`, `all` or `only`; only `only` carries `targets`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobScope {
    pub kind: String,
    #[serde(default)]
    pub targets: Vec<LeanPluginJobTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobSpawn {
    pub slot: String,
    pub attempt: u32,
    pub target: LeanPluginJobTarget,
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobTarget {
    pub node: String,
    pub agent: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobTargetHop {
    pub target: LeanPluginJobTarget,
    pub hop: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWireBinding {
    pub wire: String,
    pub target: LeanPluginJobTarget,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWithin {
    pub base: String,
    pub cwd: String,
}
