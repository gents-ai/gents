use super::*;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct LeanEventDeliveryWorld {
    pub persistent_set: Vec<String>,
    pub subscription_queue: Vec<String>,
    pub processed_set: Vec<String>,
    pub handled: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanEventDeliveryAction {
    Persist { doc: String },
    Depersist { doc: String },
    Enqueue { doc: String },
    Drop { doc: String },
    DeliverFromQueue { doc: String },
    RescanTick,
    Handle { doc: String },
    Release { doc: String },
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanEventDeliveryTransitionCase {
    pub name: String,
    pub pre: LeanEventDeliveryWorld,
    pub action: LeanEventDeliveryAction,
    pub post: LeanEventDeliveryWorld,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanEventDeliverySourceInstance {
    pub name: String,
    pub dedupe_policy: String,
    pub rescan_bounded_by: u64,
    pub deviation: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanEventDeliveryConvergenceTrace {
    pub name: String,
    pub instance_name: String,
    pub initial_world: LeanEventDeliveryWorld,
    pub actions: Vec<LeanEventDeliveryAction>,
    pub final_world: LeanEventDeliveryWorld,
    pub status: String,
}
