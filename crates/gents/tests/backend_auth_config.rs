mod support;

use std::ffi::OsString;
use std::sync::LazyLock;

use support::fixtures::test_behavior;

static ENV_VAR_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

struct TestEnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl TestEnvGuard {
    fn new(names: &[&'static str]) -> Self {
        let saved = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        Self { saved }
    }

    fn set(&mut self, name: &'static str, value: &str) {
        unsafe {
            std::env::set_var(name, value);
        }
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.iter().rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

#[test]
fn behavior_config_prefers_raw_backend_api_key() {
    let _env_guard = ENV_VAR_LOCK.blocking_lock();
    let mut behavior = test_behavior("behavior-raw", "backend-raw", Some("IGNORED_ENV_KEY"));
    behavior.backend_api_key = Some("raw-key".to_string());

    let mut env = TestEnvGuard::new(&["IGNORED_ENV_KEY"]);
    env.set("IGNORED_ENV_KEY", "env-key");
    let resolved = behavior.resolve_backend_api_key().expect("resolve api key");

    assert_eq!(resolved.as_deref(), Some("raw-key"));
}

#[test]
fn behavior_config_prefers_backend_specific_api_key_env_var() {
    let _env_guard = ENV_VAR_LOCK.blocking_lock();
    let behavior = test_behavior("behavior-a", "backend-a", Some("GENTS_TEST_BACKEND_KEY"));

    let mut env = TestEnvGuard::new(&["GENTS_TEST_BACKEND_KEY"]);
    env.set("GENTS_TEST_BACKEND_KEY", "backend-key");
    let resolved = behavior.resolve_backend_api_key().expect("resolve api key");

    assert_eq!(resolved.as_deref(), Some("backend-key"));
}
