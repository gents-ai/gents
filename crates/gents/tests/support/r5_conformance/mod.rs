pub mod invariants;
pub mod runner;
pub mod scenario;

pub fn init_tracing() {
    const DEFAULT_FILTER: &str = "gents::trigger_engine::subagent_source=trace,gents::trigger_engine::production_materializer=debug,gents::agent::p2p_reconcile::enrollment_reconcile=debug,gents::agent::runtime::control_watcher=info";
    // A helper that panics instead of returning an error loses the runner's
    // action context, so the modeled boundary has to reach the captured test
    // output before the action runs.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("{DEFAULT_FILTER},{}=info", module_path!()))
    });
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(filter)
        .try_init();
}

#[allow(unused_imports)]
pub use runner::Harness;
#[allow(unused_imports)]
pub use scenario::NodeId;
