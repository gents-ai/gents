/// An access policy the runtime adds to every home it opens. The bundled SDL
/// names one by its [`BuiltInPolicy::placeholder`], never by its ID: a home's
/// [`HomePolicyIds`] fills the IDs in when the schema is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltInPolicy {
    Household,
    Mailbox,
    ClientJournal,
    ToolFileChange,
}

impl BuiltInPolicy {
    /// In the order a home adds them.
    pub const ALL: [Self; 4] = [
        Self::Household,
        Self::Mailbox,
        Self::ClientJournal,
        Self::ToolFileChange,
    ];

    pub const fn definition(self) -> &'static str {
        match self {
            Self::Household => crate::AGENT_OWNERSHIP_POLICY,
            Self::Mailbox => crate::MAILBOX_POLICY,
            Self::ClientJournal => crate::CLIENT_JOURNAL_POLICY,
            Self::ToolFileChange => crate::TOOL_FILE_CHANGE_POLICY,
        }
    }

    /// The `@policy(id:)` value the bundled SDL carries in place of the ID.
    pub const fn placeholder(self) -> &'static str {
        match self {
            Self::Household => "$household",
            Self::Mailbox => "$mailbox",
            Self::ClientJournal => "$client_journal",
            Self::ToolFileChange => "$tool_file_change",
        }
    }
}

/// The ID of each built-in policy in one home. Every reader of a built-in
/// policy's ID, and every load of the bundled SDL, takes it from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePolicyIds {
    household: &'static str,
    mailbox: &'static str,
    client_journal: &'static str,
    tool_file_change: &'static str,
}

impl HomePolicyIds {
    /// The IDs of a home on the node's own store
    /// (`DocumentAcpChoice.PolicyIds.counter`): each policy is added under
    /// counter 0, so every such home holds the same IDs.
    pub fn node_store() -> &'static Self {
        const NODE_STORE: HomePolicyIds = HomePolicyIds {
            household: "4ea18f610525e2476625b09905063701daf3d1fe757ec8784f955e2a61e01b7c",
            mailbox: "10f15ba2901821f2265d8dccc2cc49878960a5d99a489e1c6a03a7c0767dc0f2",
            client_journal: "0db680be49781d06d5793ca0eecc8fad2b1a5d05467c535325ddeb148f679b9c",
            tool_file_change: "2e14bb0746cf90a5e955db6b9bb5d9098f6aa0776849c8bcb68db4dd1e67ea82",
        };
        &NODE_STORE
    }

    /// The IDs of a home on an embedded Vera node
    /// (`DocumentAcpChoice.PolicyIds.seeded`): the policies its deployment is
    /// seeded with, under one creator and a fixed nonce each, so every such
    /// home holds the same IDs. gents-store checks them against the seeds.
    pub fn embedded_vera() -> &'static Self {
        const EMBEDDED_VERA: HomePolicyIds = HomePolicyIds {
            household: "0f052d0eeb2d8b274b3e1dc8f9bf473b5f6afce96aa0920ca3f489fff4777c28",
            mailbox: "5324b84ea41dc068f4d6d29428ace86915d450831862bbb25c3e322c09b603af",
            client_journal: "892a0e58078046d272f53661a1d1ec4091d13948122b4dc82a14eeddb3955618",
            tool_file_change: "da40747a0af6694036fad9782e338525e2ad60b41e1eebd234c22916c2beff53",
        };
        &EMBEDDED_VERA
    }

    pub fn id(&self, policy: BuiltInPolicy) -> &str {
        match policy {
            BuiltInPolicy::Household => self.household,
            BuiltInPolicy::Mailbox => self.mailbox,
            BuiltInPolicy::ClientJournal => self.client_journal,
            BuiltInPolicy::ToolFileChange => self.tool_file_change,
        }
    }

    /// `sdl` with each built-in policy's placeholder replaced by this home's
    /// ID. Any other policy ID is left as written.
    pub fn render(&self, sdl: &str) -> String {
        let mut rendered = sdl.to_owned();
        for policy in BuiltInPolicy::ALL {
            rendered = rendered.replace(
                &format!("id: \"{}\"", policy.placeholder()),
                &format!("id: \"{}\"", self.id(policy)),
            );
        }
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::{BuiltInPolicy, HomePolicyIds};

    /// Every SDL file the build script bundles, as written.
    fn bundled_sdl() -> Vec<(std::path::PathBuf, String)> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for dir in [
            "schemas/agent",
            "../gents-protocol/schemas/inference",
            "../gents-protocol/schemas/services",
        ] {
            for entry in std::fs::read_dir(manifest.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|ext| ext == "graphql") {
                    let sdl = std::fs::read_to_string(&path).unwrap();
                    files.push((path, sdl));
                }
            }
        }
        assert!(files.len() >= crate::ALL.len());
        files
    }

    #[test]
    fn every_placeholder_in_the_bundle_is_rendered_and_nothing_else_changes() {
        for (ids, (path, sdl)) in id_sets()
            .into_iter()
            .flat_map(|ids| bundled_sdl().into_iter().map(move |file| (ids, file)))
        {
            let rendered = ids.render(&sdl);
            assert!(!rendered.contains("id: \"$"), "{}", path.display());
            let mut restored = rendered.clone();
            for policy in BuiltInPolicy::ALL {
                restored = restored.replace(
                    &format!("id: \"{}\"", ids.id(policy)),
                    &format!("id: \"{}\"", policy.placeholder()),
                );
            }
            assert_eq!(restored, sdl, "{}", path.display());
        }
    }

    #[test]
    fn the_bundle_names_no_built_in_policy_by_its_id() {
        for ids in id_sets() {
            for (path, sdl) in bundled_sdl() {
                for policy in BuiltInPolicy::ALL {
                    assert!(!sdl.contains(ids.id(policy)), "{}", path.display());
                }
            }
        }
    }

    fn id_sets() -> [&'static HomePolicyIds; 2] {
        [HomePolicyIds::node_store(), HomePolicyIds::embedded_vera()]
    }

    #[test]
    fn the_two_id_sets_share_no_id() {
        for policy in BuiltInPolicy::ALL {
            for other in BuiltInPolicy::ALL {
                assert_ne!(
                    HomePolicyIds::node_store().id(policy),
                    HomePolicyIds::embedded_vera().id(other)
                );
            }
        }
    }
}
