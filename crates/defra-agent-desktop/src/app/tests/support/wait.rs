pub(crate) fn init_test_tracing() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let filter = std::env::var("DEFRA_AGENT_DESKTOP_TEST_LOG")
            .unwrap_or_else(|_| "warn,defra_agent_desktop::app::tests=info".to_string());
        let _ = tracing_subscriber::registry()
            .with(with_default_transport_noise_filters(EnvFilter::new(filter)))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact()
                    .without_time(),
            )
            .with(global_log_layer())
            .try_init();
    });
}

pub(crate) fn with_default_transport_noise_filters(filter: EnvFilter) -> EnvFilter {
    filter
        .add_directive(
            "iroh_quinn_proto::connection=error"
                .parse()
                .expect("valid tracing directive"),
        )
        .add_directive(
            "noq_proto::connection=error"
                .parse()
                .expect("valid tracing directive"),
        )
}

pub(crate) fn live_desktop_test_guard() -> MutexGuard<'static, ()> {
    static LIVE_DESKTOP_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LIVE_DESKTOP_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("live desktop test lock poisoned")
}

pub(crate) fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn graphql_optional_string_field(name: &str, value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

pub(crate) fn assert_logs_filter_has_results(texts: &[String]) {
    assert!(
        !texts.iter().any(|text| text.contains("No Matching Events")),
        "logs filter unexpectedly rendered empty state"
    );
}

pub(crate) fn wait_for_value<T>(
    label: &str,
    timeout: Duration,
    mut loader: impl FnMut() -> Option<T>,
) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = loader() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {label}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
