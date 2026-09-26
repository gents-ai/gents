use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;

use super::format::{append_entry, write_pack_with};
use super::*;
use crate::pack::{declared_paths, validate_manifest};

/// Writes one of this build's own bundled packs out as a directory, so the
/// tests work on a real pack rather than a fixture that could drift from
/// what a pack actually looks like.
fn bundled_pack_dir(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let pack = crate::pack::resolve_pack(name).expect("a bundled pack");
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(name);
    for path in declared_paths(&pack.manifest) {
        let target = root.join(&path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&target, pack.asset(&path).expect("asset")).expect("write");
    }
    (dir, root)
}

/// A raw `.pack` with exactly these entries, in this order, for building
/// files the writer would never produce.
fn raw_pack(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in entries {
        append_entry(&mut builder, path, data.len() as u64, *data).expect("append");
    }
    let tar_bytes = builder.into_inner().expect("tar bytes");
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&tar_bytes).expect("write");
    gz.finish().expect("finish")
}

/// The entries of a real pack, as (path, bytes), in file order.
fn entries_of(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    archive
        .entries()
        .expect("entries")
        .map(|entry| {
            let mut entry = entry.expect("entry");
            let path = entry.path().unwrap().to_str().unwrap().to_owned();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data).unwrap();
            (path, data)
        })
        .collect()
}

fn rebuild(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();
    raw_pack(&refs)
}

#[test]
fn a_packed_pack_reads_back_as_the_same_pack() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, _) = pack_dir(&root).expect("packing");
    let packed = PackArchive::from_bytes(&bytes).expect("reading back");

    let bundled = crate::pack::resolve_pack("mailbox").expect("a bundled pack");
    assert_eq!(packed.manifest().name, bundled.manifest.name);
    assert_eq!(packed.manifest().version, bundled.manifest.version);
    for path in declared_paths(&bundled.manifest) {
        assert_eq!(
            packed.asset(&path).expect("packed asset"),
            bundled.asset(&path).expect("bundled asset"),
            "{path} differs"
        );
    }
}

#[test]
fn every_bundled_pack_keeps_its_digest_through_a_pack_file() {
    for name in crate::pack::BUNDLED_PACK_NAMES {
        let (_guard, root) = bundled_pack_dir(name);
        let (bytes, header) = pack_dir(&root).expect("packing");
        let bundled = crate::pack::resolve_pack(name).expect("a bundled pack");
        assert_eq!(header.digest, bundled.digest, "{name}");
        assert_eq!(
            PackArchive::from_bytes(&bytes).expect("reading").digest(),
            bundled.digest,
            "{name}"
        );
    }
}

#[test]
fn packing_twice_gives_the_same_bytes() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (first, first_header) = pack_dir(&root).expect("first");
    let (second, second_header) = pack_dir(&root).expect("second");
    assert_eq!(
        first, second,
        "a rebuild that changes nothing changes no bytes"
    );
    assert_eq!(first_header, second_header);
}

#[test]
fn a_pack_is_a_tar_gz_any_archive_tool_can_read_with_its_header_first() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, header) = pack_dir(&root).expect("packing");
    let entries = entries_of(&bytes);
    assert_eq!(entries[0].0, HEADER_ENTRY);
    let on_disk: PackHeader = serde_json::from_slice(&entries[0].1).expect("header json");
    assert_eq!(on_disk, header);
    assert_eq!(on_disk.format, FORMAT);
    assert_eq!(on_disk.format_version, FORMAT_VERSION);
    assert_eq!(on_disk.coordinate, "gents/mailbox");
    let rest: Vec<&str> = entries[1..].iter().map(|(path, _)| path.as_str()).collect();
    let bundled = crate::pack::resolve_pack("mailbox").unwrap();
    assert_eq!(
        rest,
        declared_paths(&bundled.manifest),
        "entries follow the digest order, so a reader hashes while streaming"
    );
    assert_eq!(
        header.file_name(),
        format!("gents.mailbox-{}.pack", bundled.manifest.version)
    );
}

#[test]
fn a_file_without_the_header_first_is_not_a_pack() {
    let error = PackArchive::from_bytes(&raw_pack(&[("README.md", b"# not a pack")]))
        .expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("does not start with pack.json"),
        "{error:#}"
    );
}

#[test]
fn a_changed_asset_fails_the_digest_the_header_claims() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, _) = pack_dir(&root).expect("packing");
    let mut entries = entries_of(&bytes);
    let asset = entries
        .iter_mut()
        .rev()
        .find(|(path, _)| path != "manifest.json")
        .expect("an asset");
    asset.1.push(b'!');
    let error = PackArchive::from_bytes(&rebuild(&entries)).expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("but the contents hash to"),
        "{error:#}"
    );
}

#[test]
fn a_header_that_misnames_the_pack_is_refused() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, header) = pack_dir(&root).expect("packing");
    let mut entries = entries_of(&bytes);
    let lie = PackHeader {
        version: "9.9.9".into(),
        ..header
    };
    entries[0].1 = serde_json::to_vec(&lie).unwrap();
    let error = PackArchive::from_bytes(&rebuild(&entries)).expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("but the manifest is"),
        "{error:#}"
    );
}

#[test]
fn a_newer_format_version_is_named_not_misread() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, _) = pack_dir(&root).expect("packing");
    let mut entries = entries_of(&bytes);
    let mut header: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
    header["format_version"] = 2.into();
    entries[0].1 = serde_json::to_vec(&header).unwrap();
    let error = PackArchive::from_bytes(&rebuild(&entries)).expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("format version 2"),
        "{error:#}"
    );
}

#[test]
fn entries_out_of_digest_order_are_refused() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, _) = pack_dir(&root).expect("packing");
    let mut entries = entries_of(&bytes);
    let n = entries.len();
    entries.swap(n - 1, n - 2);
    let error = PackArchive::from_bytes(&rebuild(&entries)).expect_err("must be refused");
    assert!(format!("{error:#}").contains("out of order"), "{error:#}");
}

#[test]
fn a_member_the_manifest_does_not_declare_is_refused() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    std::fs::write(root.join("stowaway.md"), b"not declared").expect("write");
    let (bytes, _) = pack_dir(&root).expect("packing");
    assert!(
        PackArchive::from_bytes(&bytes)
            .expect("reading")
            .asset("stowaway.md")
            .is_err(),
        "packing ignores an undeclared file, because the manifest is the description"
    );

    let mut entries = entries_of(&bytes);
    entries.push(("zzz_stowaway.md".to_owned(), b"not declared".to_vec()));
    let error = PackArchive::from_bytes(&rebuild(&entries)).expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("does not declare"),
        "{error:#}"
    );
}

#[test]
fn a_missing_declared_asset_is_refused_when_the_pack_is_built() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).expect("manifest"))
            .expect("parse");
    let dropped = manifest.metadata.assets[0].clone();
    std::fs::remove_file(root.join(&dropped)).expect("remove");
    let error = pack_dir(&root).expect_err("must be refused");
    assert!(format!("{error:#}").contains(&dropped), "{error:#}");
}

#[test]
fn a_path_that_escapes_the_pack_is_refused() {
    // `tar::Header::set_path` refuses a `..` component outright, so a hostile
    // entry is written at the raw-byte level to prove this reader's own check
    // refuses it too.
    let mut header = tar::Header::new_ustar();
    header.set_size(4);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    let name = b"../escape.txt";
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_cksum();

    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, _) = pack_dir(&root).expect("packing");
    let entries = entries_of(&bytes);
    let mut builder = tar::Builder::new(Vec::new());
    append_entry(
        &mut builder,
        HEADER_ENTRY,
        entries[0].1.len() as u64,
        entries[0].1.as_slice(),
    )
    .expect("append");
    builder
        .append(&header, &b"nope"[..])
        .expect("append escaping entry");
    let tar_bytes = builder.into_inner().expect("tar bytes");
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&tar_bytes).expect("write");
    let bytes = gz.finish().expect("finish");

    let error = PackArchive::from_bytes(&bytes).expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("is not a path a pack may carry"),
        "{error:#}"
    );
}

#[test]
fn every_bound_is_enforced_on_read_and_write() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, _) = pack_dir(&root).expect("packing");
    let bounds = Bounds::default();
    for (tight, expected) in [
        (
            Bounds {
                compressed: bytes.len() as u64 - 1,
                ..bounds
            },
            "compressed bytes",
        ),
        (
            Bounds {
                decompressed: 16,
                ..bounds
            },
            "decompressed bytes",
        ),
        (
            Bounds {
                entries: 1,
                ..bounds
            },
            "more than 1 entries",
        ),
    ] {
        let error = PackArchive::from_reader(bytes.as_slice(), tight).expect_err("read bound");
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    for (tight, expected) in [
        (
            Bounds {
                compressed: 16,
                ..bounds
            },
            "byte bound",
        ),
        (
            Bounds {
                decompressed: 16,
                ..bounds
            },
            "byte bound",
        ),
        (
            Bounds {
                entries: 1,
                ..bounds
            },
            "entry bound",
        ),
    ] {
        let error = write_pack_with(&root, tight).expect_err("write bound");
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
}

#[test]
fn writing_a_pack_out_reproduces_the_directory_it_came_from() {
    let (_guard, root) = bundled_pack_dir("mailbox");
    let (bytes, header) = pack_dir(&root).expect("packing");
    let packed = PackArchive::from_bytes(&bytes).expect("reading");
    let out = tempfile::tempdir().expect("tempdir");
    packed.write_to(out.path()).expect("writing out");
    assert!(
        !out.path().join(HEADER_ENTRY).exists(),
        "the header is not a pack file"
    );
    let (again, again_header) = pack_dir(out.path()).expect("repacking");
    assert_eq!(
        again, bytes,
        "a pack written to disk and packed again is the same pack"
    );
    assert_eq!(again_header, header);
}

#[test]
fn a_digest_is_only_ever_sha256_hex() {
    let good = format!("sha256:{}", "a".repeat(64));
    assert_eq!(digest_hex(&good).unwrap(), "a".repeat(64));
    for bad in [
        "",
        "sha256:",
        "md5:abc",
        &format!("sha256:{}", "A".repeat(64)),
        &format!("sha256:{}", "a".repeat(63)),
    ] {
        assert!(digest_hex(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_plugin_pack_carries_its_artifact_and_asks_for_what_the_plugin_asks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("shipping_plugins");
    std::fs::create_dir_all(root.join("plugins")).expect("mkdir");
    std::fs::write(root.join("README.md"), b"# shipping plugins").expect("write");
    std::fs::write(root.join("plugins/format_check.afb"), b"not a real afb yet").expect("write");
    let manifest = serde_json::json!({
        "manifest_version": 1,
        "name": "shipping_plugins",
        "version": "0.1.0",
        "description": "A pack that is nothing but capabilities",
        "authors": ["gents-ai contributors"],
        "tags": ["plugins"],
        "kind": "plugins",
        "assets": ["README.md", "plugins/format_check.afb"],
        "plugins": [{
            "name": "format_check",
            "description": "Checks formatting and says what is wrong",
            "artifact": "plugins/format_check.afb",
            "language": "rust",
            "input_schema": {"type": "object", "properties": {}},
            "manifold": {"fs": {"ReadOnly": ["/workspace"]}, "net": "None", "env": "None",
                         "crypto": false, "child_process": false, "allow_exit": false,
                         "http_timeout_ms": null}
        }]
    });
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("encode"),
    )
    .expect("write");

    let (bytes, _) = pack_dir(&root).expect("packing a plugins pack");
    let packed = PackArchive::from_bytes(&bytes).expect("reading it back");
    assert_eq!(packed.plugins().len(), 1);
    assert_eq!(packed.plugins()[0].name, "format_check");
    assert_eq!(
        packed
            .plugin_artifact("format_check")
            .expect("the artifact"),
        b"not a real afb yet",
        "a plugin's compiled artifact travels inside the pack"
    );
}

#[test]
fn a_plugin_whose_artifact_is_not_a_declared_asset_is_refused() {
    let mut manifest: crate::pack::PackManifest = serde_json::from_value(serde_json::json!({
        "manifest_version": 1,
        "name": "shipping_plugins",
        "version": "0.1.0",
        "description": "A pack that is nothing but capabilities",
        "authors": ["gents-ai contributors"],
        "tags": ["plugins"],
        "kind": "plugins",
        "assets": ["README.md"],
        "plugins": [{
            "name": "format_check",
            "description": "Checks formatting",
            "artifact": "plugins/format_check.afb",
            "language": "rust",
            "input_schema": {"type": "object"}
        }]
    }))
    .expect("a manifest");
    let error = validate_manifest("shipping_plugins", &manifest).expect_err("must be refused");
    assert!(
        format!("{error:#}").contains("not its artifact"),
        "{error:#}"
    );

    manifest
        .metadata
        .assets
        .push("plugins/format_check.afb".to_owned());
    validate_manifest("shipping_plugins", &manifest).expect("and accepted once declared");
}
