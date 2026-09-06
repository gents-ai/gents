/// Filesystem spelling is not document identity. Kebab-case IDs remain
/// unchanged in the database; their authored/exported handles use underscores.
pub(crate) fn document_handle(id: &str) -> String {
    id.replace('-', "_")
}
