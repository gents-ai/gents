#[cfg(test)]
const CAPTURED_EVENT_TARGETS: &[&str] = &[
    gents_store::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET,
    crate::runtime::status::RECONCILE_PHASE_EVENT_TARGET,
    gents_session::session::REFUSAL_PROJECTION_EVENT_TARGET,
];

#[cfg(test)]
pub(crate) fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(CapturedTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

#[cfg(test)]
struct CapturedTargetSubscriber;

#[cfg(test)]
impl tracing::Subscriber for CapturedTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        CAPTURED_EVENT_TARGETS.contains(&metadata.target())
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}
