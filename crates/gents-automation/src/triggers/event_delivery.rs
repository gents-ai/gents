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

#[cfg(test)]
#[test]
fn generated_source_contracts_match_runtime() {
    use gents_lean_vocab::families::event_delivery_source_contracts as family;
    let cases = family::cases::<String>();
    let sources = runtime_event_delivery_source_contracts();
    assert_eq!(sources.len(), cases.len());
    for case in cases {
        let name = match case.input.as_str() {
            "watcher" => DefraWatcher::EVENT_DELIVERY_CONTRACT.name,
            "event_source" => EventSource::EVENT_DELIVERY_CONTRACT.name,
            other => panic!("unknown source {other}"),
        };
        let source = sources.iter().find(|source| source.name == name).unwrap();
        case.check(family::SourceContract {
            name: source.name.to_owned(),
            dedupe_policy: source.dedupe_policy.to_owned(),
            rescan_bounded_by: source.rescan_bounded_by,
            deviation: source.deviation.map(str::to_owned),
        });
    }
}
