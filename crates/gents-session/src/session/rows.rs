pub(crate) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}
