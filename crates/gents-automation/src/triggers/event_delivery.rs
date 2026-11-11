use crate::triggers::engine::event_source::EventSource;
use gents_session::request::event_delivery::{
    EventDeliveryRuntimeContract, EventDeliverySourceContract,
};
use gents_session::request::watcher::DefraWatcher;

pub fn runtime_event_delivery_source_contracts() -> [EventDeliverySourceContract; 2] {
    [
        DefraWatcher::EVENT_DELIVERY_CONTRACT,
        EventSource::EVENT_DELIVERY_CONTRACT,
    ]
}
