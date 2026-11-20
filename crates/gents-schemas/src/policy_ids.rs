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
    /// The IDs every home holds while a policy's ID is a function of its
    /// definition alone: each is added under counter 0, so two homes cannot
    /// tell their policies apart (gents#3831).
    pub fn shared() -> &'static Self {
        const SHARED: HomePolicyIds = HomePolicyIds {
            household: "fd59b3cc10193581adb497fd6d55c7e740f4866f720af6f18da780248ce26a5c",
            mailbox: "10f15ba2901821f2265d8dccc2cc49878960a5d99a489e1c6a03a7c0767dc0f2",
            client_journal: "0db680be49781d06d5793ca0eecc8fad2b1a5d05467c535325ddeb148f679b9c",
            tool_file_change: "2e14bb0746cf90a5e955db6b9bb5d9098f6aa0776849c8bcb68db4dd1e67ea82",
        };
        &SHARED
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
        let ids = HomePolicyIds::shared();
        for (path, sdl) in bundled_sdl() {
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
        let ids = HomePolicyIds::shared();
        for (path, sdl) in bundled_sdl() {
            for policy in BuiltInPolicy::ALL {
                assert!(!sdl.contains(ids.id(policy)), "{}", path.display());
            }
        }
    }
}
