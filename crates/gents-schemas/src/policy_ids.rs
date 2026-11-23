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
    transport_v2: bool,
}

impl HomePolicyIds {
    /// The IDs of a home on the node's own store
    /// (`DocumentAcpChoice.PolicyIds.counter`): each policy is added under
    /// counter 0, so every such home holds the same IDs.
    pub fn node_store() -> &'static Self {
        const NODE_STORE: HomePolicyIds = HomePolicyIds {
            household: "59fbf3a7eee69609a15e33145960620cf338b1bd62e16e10198b1982d0dede2c",
            mailbox: "10f15ba2901821f2265d8dccc2cc49878960a5d99a489e1c6a03a7c0767dc0f2",
            client_journal: "0db680be49781d06d5793ca0eecc8fad2b1a5d05467c535325ddeb148f679b9c",
            tool_file_change: "2e14bb0746cf90a5e955db6b9bb5d9098f6aa0776849c8bcb68db4dd1e67ea82",
            transport_v2: false,
        };
        &NODE_STORE
    }

    /// The IDs of a home on an embedded Vera node
    /// (`DocumentAcpChoice.PolicyIds.seeded`): the policies its deployment is
    /// seeded with, under one creator and a fixed nonce each, so every such
    /// home holds the same IDs. gents-store checks them against the seeds.
    pub fn embedded_vera() -> &'static Self {
        const EMBEDDED_VERA: HomePolicyIds = HomePolicyIds {
            household: "e476a9ca6d3084a07b3ee70474ab0c3c1ed7ae50049ba42bf299c9391103fc20",
            mailbox: "5324b84ea41dc068f4d6d29428ace86915d450831862bbb25c3e322c09b603af",
            client_journal: "892a0e58078046d272f53661a1d1ec4091d13948122b4dc82a14eeddb3955618",
            tool_file_change: "da40747a0af6694036fad9782e338525e2ad60b41e1eebd234c22916c2beff53",
            transport_v2: false,
        };
        &EMBEDDED_VERA
    }

    pub fn embedded_vera_transport_v2() -> &'static Self {
        static V2: HomePolicyIds = HomePolicyIds {
            household: "8a1056bb86c3b1589858e977696432b78b5071ff9a6432433223826c50b2fbd0",
            mailbox: "5324b84ea41dc068f4d6d29428ace86915d450831862bbb25c3e322c09b603af",
            client_journal: "892a0e58078046d272f53661a1d1ec4091d13948122b4dc82a14eeddb3955618",
            tool_file_change: "da40747a0af6694036fad9782e338525e2ad60b41e1eebd234c22916c2beff53",
            transport_v2: true,
        };
        &V2
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
        if self.transport_v2 {
            for resource in ["discovery", "scoped_discovery", "enrollment_records"] {
                let before = format!("resource: \"{resource}\") @branchable {{");
                let after = format!(
                    "resource: \"{resource}\", parentField: \"publisher_node_id\") @branchable {{\n    publisher_node_id: String!"
                );
                rendered = rendered.replace(&before, &after);
            }
        }
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

    #[test]
    fn transport_v2_selects_parent_gates_without_changing_legacy_sdl() {
        let legacy = HomePolicyIds::embedded_vera();
        let v2 = HomePolicyIds::embedded_vera_transport_v2();
        assert_ne!(
            legacy.id(BuiltInPolicy::Household),
            v2.id(BuiltInPolicy::Household)
        );
        let mut gated = 0;
        for (path, sdl) in bundled_sdl() {
            let v2_sdl = v2.render(&sdl);
            let old_sdl = legacy.render(&sdl);
            let transport = ["discovery", "scoped_discovery", "enrollment_records"]
                .iter()
                .any(|resource| sdl.contains(&format!("resource: \"{resource}\") @branchable {{")));
            gated += usize::from(transport);
            assert_eq!(
                v2_sdl.contains("parentField: \"publisher_node_id\""),
                transport,
                "{}",
                path.display()
            );
            assert!(
                !old_sdl.contains("parentField: \"publisher_node_id\""),
                "{}",
                path.display()
            );
            assert_eq!(
                v2_sdl.contains("publisher_node_id: String!"),
                transport,
                "{}",
                path.display()
            );
        }
        assert_eq!(gated, 9);
    }

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
