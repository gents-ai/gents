pub(super) fn is_enabled(success: bool, state: &str) -> bool {
    success && matches!(state, "enabled" | "enabled-runtime")
}

#[cfg(test)]
mod tests {
    use super::is_enabled;

    #[test]
    fn recognizes_persistent_and_runtime_enablement() {
        assert!(is_enabled(true, "enabled"));
        assert!(is_enabled(true, "enabled-runtime"));
        for state in ["disabled", "masked", "masked-runtime", "static"] {
            assert!(!is_enabled(false, state), "{state}");
        }
        assert!(!is_enabled(false, "enabled-runtime"));
    }
}
