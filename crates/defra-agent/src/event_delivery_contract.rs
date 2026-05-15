//! Runtime-facing contract metadata for the Lean EventDelivery model.

use crate::trigger_engine::{event_source::EventSource, subagent_source::SubagentSource};
use crate::watcher::DefraWatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventDeliverySourceContract {
    pub name: &'static str,
    pub dedupe_policy: &'static str,
    pub rescan_bounded_by: u64,
    pub deviation: Option<&'static str>,
}

pub trait EventDeliveryRuntimeContract {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract;
}

pub fn runtime_event_delivery_source_contracts() -> [EventDeliverySourceContract; 3] {
    [
        DefraWatcher::EVENT_DELIVERY_CONTRACT,
        EventSource::EVENT_DELIVERY_CONTRACT,
        SubagentSource::EVENT_DELIVERY_CONTRACT,
    ]
}
