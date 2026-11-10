use super::{Path, PathBuf, Request};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Queue {
    Unscoped,
    Workspace,
}

impl Queue {
    pub(super) const ALL: [Self; 2] = [Self::Unscoped, Self::Workspace];

    pub(super) fn for_request(request: &Request) -> Self {
        if request.binding().is_some() {
            Self::Workspace
        } else {
            Self::Unscoped
        }
    }

    pub(super) fn directory(self, home: &Path) -> PathBuf {
        home.join(crate::store::local::home::PLUGIN_APPROVALS_DIR_NAME)
            .join(match self {
                Self::Unscoped => "v2",
                Self::Workspace => "v3",
            })
    }

    pub(super) fn request_file(self, home: &Path, id: &str) -> PathBuf {
        self.directory(home).join(format!("{id}.json"))
    }

    pub(super) fn decision_file(self, home: &Path, id: &str) -> PathBuf {
        self.directory(home).join(format!("{id}.decision"))
    }

    pub(super) fn listener_file(self, home: &Path) -> PathBuf {
        self.directory(home).join(".listening")
    }

    pub(super) fn matches(self, request: &Request) -> bool {
        self == Self::for_request(request)
    }
}
