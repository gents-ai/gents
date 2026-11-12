use std::collections::BTreeMap;
use std::path::Path;

use crate::pack::archive::{pack_dir, PackArchive};
use crate::pack::PackSource;

#[tokio::test]
async fn asset_install_keeps_portable_source_across_reinstall() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/tests/fixtures/packs/assets_fixture");
    let (bytes, _) = pack_dir(&fixture).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();
    let home = tempfile::tempdir().unwrap();
    let local = PackSource::local(Path::new("/private/folder/assets.pack"));
    for source in [local.clone(), PackSource::Store] {
        crate::pack::plugin_lifecycle::install(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::new(),
            crate::pack::InstalledVia::Operator,
            source,
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap();
    }
    let record = crate::pack::read_home_install(home.path(), "fixture/assets_fixture")
        .unwrap()
        .unwrap();
    assert_eq!(record.source, Some(PackSource::Store));
    assert_eq!(record.source_history[0].source, Some(local));
    assert_eq!(record.source_history[0].digest, archive.digest());
    assert!(!serde_json::to_string(&record)
        .unwrap()
        .contains("/private/folder"));
}
