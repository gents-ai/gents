use std::io::Write;
use std::sync::{Arc, Mutex};
use super::{HostRecordStore, ProcessIdentity, ProcessRecord};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

fn capture<T>(operation: impl FnOnce() -> T) -> (T, String) {
    crate::test_support::root::enable_scoped_event_capture();
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .with_writer(output.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, operation);
    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    (result, text)
}

#[test]
fn missing_directory_is_empty_without_warning_for_both_read_modes() {
    let directory = tempfile::tempdir().unwrap();
    let store = HostRecordStore::<ProcessRecord>::Durable(directory.path().join("missing"));
    let ((), output) = capture(|| {
        assert!(store.list().is_empty());
        assert!(store.try_list().unwrap().is_empty());
    });
    assert!(output.is_empty(), "{output}");
}

#[test]
fn unreadable_directory_warns_in_recovery_and_refuses_strict_verification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-a-directory");
    std::fs::write(&path, b"blocked").unwrap();
    let store = HostRecordStore::<ProcessRecord>::Durable(path.clone());
    let ((), output) = capture(|| {
        assert!(store.list().is_empty());
        assert!(store.try_list().is_err());
    });
    assert!(
        output.contains("WARN"),
        "directory failure was silent: {output}"
    );
    assert!(
        output.contains(path.to_str().unwrap()),
        "directory is missing from the diagnostic: {output}"
    );
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
