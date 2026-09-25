pub mod invariants;
pub mod runner;
pub mod scenario;

pub fn init_tracing() {
    const DEFAULT_FILTER: &str = "gents::trigger_engine::subagent_source=trace,gents::trigger_engine::production_materializer=debug,gents::agent::p2p_reconcile::enrollment_reconcile=debug,gents::agent::runtime::control_watcher=info";
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(filter)
        .try_init();
}

#[allow(unused_imports)]
pub use runner::Harness;
#[allow(unused_imports)]
pub use scenario::NodeId;
