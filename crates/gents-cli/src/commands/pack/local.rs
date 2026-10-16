pub(super) use gents::pack::local::{classify, open, LocalName};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn only_an_explicit_path_or_digest_is_local() {
        assert_eq!(classify("mailbox"), None);
        assert_eq!(classify("acme/mailbox"), None);
        assert_eq!(
            classify("sha256:abc"),
            Some(LocalName::Digest("sha256:abc"))
        );
        for path in [
            "./mailbox",
            "../mailbox",
            "/tmp/mailbox",
            "x.pack",
            "dir/x.pack",
        ] {
            assert_eq!(
                classify(path),
                Some(LocalName::Path(Path::new(path))),
                "{path}"
            );
        }
    }

    #[test]
    fn a_directory_a_pack_file_and_its_digest_open_as_the_same_pack() {
        let fixture = super::super::test_support::fixture_dir("assets_fixture");
        let home = tempfile::tempdir().unwrap();
        let (fixture_bytes, fixture_header) = gents::pack_archive::pack_dir(&fixture).unwrap();

        let from_dir = open(&LocalName::Path(&fixture), home.path()).unwrap();
        assert_eq!(from_dir.digest(), fixture_header.digest);

        let file = home.path().join("assets_fixture.pack");
        std::fs::write(&file, fixture_bytes).unwrap();
        let from_file = open(&LocalName::Path(&file), home.path()).unwrap();
        assert_eq!(from_file.digest(), fixture_header.digest);

        let from_digest = open(&LocalName::Digest(&fixture_header.digest), home.path()).unwrap();
        assert_eq!(from_digest.digest(), fixture_header.digest);
    }

    #[test]
    fn an_unknown_digest_names_the_store_it_looked_in() {
        let home = tempfile::tempdir().unwrap();
        let digest = format!("sha256:{}", "0".repeat(64));
        let error = open(&LocalName::Digest(&digest), home.path()).unwrap_err();
        assert!(
            format!("{error:#}").contains("is not in the pack store"),
            "{error:#}"
        );
    }
}
