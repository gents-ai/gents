use crate::request::event_delivery::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use crate::request::watcher::DefraWatcher;
use crate::triggers::engine::event_source::EventSource;

pub fn runtime_event_delivery_source_contracts() -> [EventDeliverySourceContract; 2] {
    [
        DefraWatcher::EVENT_DELIVERY_CONTRACT,
        EventSource::EVENT_DELIVERY_CONTRACT,
    ]
}
