use std::time::Duration;

/// The store's worker pool on a phone: two workers that park as soon as they
/// run out of work, a stuck-poll scan every 200 ms and pinning re-read every
/// 30 s. The pool's defaults suit a server (a worker per free core, 50 µs of
/// spinning before each park, a scan every 10 ms, a re-read every second);
/// on an idle phone they kept its cores waking with nothing to do.
pub fn pool_config() -> workers::Config {
    workers::Config {
        workers: workers::Workers::Count(2),
        idle_spin: Duration::ZERO,
        stuck_after: Duration::from_millis(200),
        recheck_every: Duration::from_secs(30),
        ..workers::Config::default()
    }
}

/// Starts the process's default pool on the phone's settings and registers
/// it. It must start before anything submits work without a pool, which
/// would otherwise start a default pool on every free core for the life of
/// the process. The registration does not keep it running: the caller holds
/// it for the life of the app.
pub fn start_default_pool() -> Result<workers::Pool, workers::Error> {
    let pool = workers::Pool::start(workers::Config {
        thread_name: "defra-default".to_owned(),
        edge_threads: Some(0),
        compaction_threads: 0,
        ..pool_config()
    })?;
    pool.make_default();
    Ok(pool)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_pool_on_the_phone_settings_starts_and_runs_work() {
        let pool = workers::Pool::start(super::pool_config()).expect("the phone settings start");
        assert_eq!(
            pool.submit_and_wait(workers::Target::LeastLoaded, || async { 6 * 7 }),
            Ok(42)
        );
        pool.shutdown_blocking().expect("the pool stops");
    }
}
