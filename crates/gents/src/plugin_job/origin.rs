use std::collections::BTreeMap;

use gents_protocol::job::PROJECT_MOUNT_CLASS;

use crate::pack::InstalledVia;

/// Lean `trustedHint`: a mount's `trusted` is true only for a pack-tier class
/// of an operator-installed pack that `trusted_packs` lists. The host still
/// decides origin at adoption.
pub fn trusted_hint(
    pack: &str,
    class: &str,
    installed_via: &BTreeMap<&str, InstalledVia>,
    trusted_packs: &[String],
) -> bool {
    class != PROJECT_MOUNT_CLASS
        && installed_via.get(pack) == Some(&InstalledVia::Operator)
        && trusted_packs.iter().any(|trusted| trusted == pack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::InstalledVia;
    use std::collections::BTreeMap;

    #[test]
    fn trusted_hint_is_operator_trusted_pack_tier_only() {
        let via: BTreeMap<&str, InstalledVia> = BTreeMap::from([
            ("acme/op", InstalledVia::Operator),
            ("acme/self", InstalledVia::SelfConfig),
        ]);
        let trusted = vec![
            "acme/op".to_owned(),
            "acme/self".to_owned(),
            "acme/ghost".to_owned(),
        ];
        assert!(trusted_hint("acme/op", "workflows", &via, &trusted));
        assert!(!trusted_hint("acme/op", "project", &via, &trusted));
        assert!(!trusted_hint("acme/self", "workflows", &via, &trusted));
        assert!(!trusted_hint("acme/ghost", "workflows", &via, &trusted));
        assert!(!trusted_hint("acme/op", "workflows", &via, &[]));
        let other = vec!["acme/other".to_owned()];
        assert!(!trusted_hint("acme/op", "workflows", &via, &other));
    }
}
