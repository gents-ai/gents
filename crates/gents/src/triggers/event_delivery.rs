use crate::triggers::engine::event_source::EventSource;
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

pub fn runtime_event_delivery_source_contracts() -> [EventDeliverySourceContract; 2] {
    [
        DefraWatcher::EVENT_DELIVERY_CONTRACT,
        EventSource::EVENT_DELIVERY_CONTRACT,
    ]
}
