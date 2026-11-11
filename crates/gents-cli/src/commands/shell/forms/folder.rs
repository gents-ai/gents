use std::path::Path;

use gents_shell::source::{FieldKind, Form, FormError, FormRef};

use super::{field, hinted, refused, value};
use crate::commands::shell::menus::grant;

/// The desktop's "Add a folder" row (`AllowedFoldersPanel.tsx:94-117`): a path typed, then added
/// read only.
pub(super) fn form() -> Form {
    Form {
        form: FormRef::AddFolder,
        title: "Add a folder".to_string(),
        note: Vec::new(),
        fields: vec![hinted(
            field("path", "Folder path", FieldKind::Text, ""),
            "/path/to/folder",
        )],
        actions: Vec::new(),
    }
}

/// Adds the path `values` name to `home`'s allowed folders, read only.
pub(super) async fn add(home: &Path, values: &[(String, String)]) -> Result<String, FormError> {
    let path = value(values, "path").trim();
    grant(home, path, "read").await.map_err(refused)
}
