use super::*;

#[test]
fn cancellation_is_scoped_to_the_window_and_observation() {
    let registry = UxWatchRegistry::default();
    let a = registry.insert("one", "a").unwrap();
    let b = registry.insert("one", "b").unwrap();
    let other = registry.insert("two", "a").unwrap();
    registry.cancel("two", "b");
    assert!(!b.is_cancelled());
    registry.cancel("one", "a");
    assert!(a.is_cancelled());
    assert!(!other.is_cancelled());
    registry.cancel_window("one");
    assert!(b.is_cancelled());
    assert!(!other.is_cancelled());
}

#[test]
fn observations_are_bounded_and_ids_cannot_be_reused() {
    let registry = UxWatchRegistry::default();
    for index in 0..16 {
        registry.insert("one", &index.to_string()).unwrap();
    }
    assert!(registry.insert("one", "extra").is_err());
    assert!(registry.insert("one", "0").is_err());
    registry.cancel("one", "0");
    assert!(registry.insert("one", "extra").is_ok());
}

/// Regression: the task checked its token and then sent outside any lock, so
/// a selection change between the two delivered the old node's snapshot to
/// the newly selected window (TLA `MCPackUxWatchUnlockedDeliver`).
#[test]
fn a_cancelled_watch_delivers_nothing() {
    let registry = UxWatchRegistry::default();
    let token = registry.insert("main", "watch").unwrap();
    let mut sent = 0;
    assert!(registry.deliver("main", "watch", || {
        sent += 1;
        true
    }));
    registry.cancel_window("main");
    assert!(token.is_cancelled());
    assert!(!registry.deliver("main", "watch", || {
        sent += 1;
        true
    }));
    assert!(!registry.deliver("main", "other", || {
        sent += 1;
        true
    }));
    assert_eq!(sent, 1);
}
