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
