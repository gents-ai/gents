//! How a pack travels: as one `.afb`.
//!
//! A pack used to exist only compiled into this binary, so `gents pack
//! list` could show nothing that did not ship with the build. To publish a
//! pack and install it somewhere else it has to become bytes, and those
//! bytes have to be checkable by whoever receives them.
//!
//! The container is the one this ecosystem already has. An `.afb` is what
//! every Afterburner tool is published and served as, so a pack that is
//! also an `.afb` needs no second format anywhere: the registry that
//! already stores, digests and serves tool packages stores, digests and
//! serves packs, and a pack that carries compiled tools is one artifact
//! rather than an archive plus a pile of modules.
//!
//! The layout inside is plain:
//!
//! ```text
//! afb.toml                          the package identity the registry indexes
//! manifold.json                     what this pack's tools may reach
//! source/manifest.json              the pack manifest
//! source/<asset>                    every asset the manifest declares,
//!                                   including each tool's compiled module
//! ```
//!
//! Two properties are the point.
//!
//! A pack installed from a registry is the same pack as the one compiled
//! into a binary. The digest is [`crate::pack::digest_declared_assets`]
//! over the declared contents in both cases, never over the container, so
//! neither the route a pack took nor the compression it arrived under can
//! change what it is.
//!
//! And a pack's tools travel inside it. A tool's compiled module is a
//! declared asset, so the pack's own digest covers it: a module cannot be
//! swapped underneath the name it was admitted under without changing the
//! pack.

use std::collections::BTreeMap;
use std::path::Path;

use afterburner_afb::{manifest, pack, Afb, Manifest};
use afterburner_core::manifold::Manifold;
use anyhow::{ensure, Context, Result};

use crate::pack::{declared_paths, validate_manifest, PackManifest, PackTool};

/// Where a pack's own files sit inside the `.afb`. Under `source/`,
/// because that is the member set the codec carries byte-exact and the one
/// an Afterburner runtime already treats as the package's bundled assets.
const SOURCE_PREFIX: &str = "source/";

/// The namespace a pack is published under when its manifest names none.
pub const DEFAULT_NAMESPACE: &str = "gents";

/// What `afb.toml` records as the language of a pack. Packs are not
/// executed as a program by the Afterburner runner; the field is required
/// by the format and this is the honest value for it.
const PACK_LANGUAGE: &str = "gents-pack";

/// A pack read out of an `.afb`.
///
/// Held in memory rather than streamed: a pack is small by the codec's own
/// bounds, every consumer wants random access to declared assets, and the
/// alternative is a temporary directory the caller has to clean up.
pub struct PackAfb {
    manifest: PackManifest,
    assets: BTreeMap<String, Vec<u8>>,
    digest: [u8; 32],
}

impl std::fmt::Debug for PackAfb {
    /// Names the pack and what it carries, never the bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackAfb")
            .field("name", &self.manifest.name)
            .field("version", &self.manifest.version)
            .field("assets", &self.assets.len())
            .field("tools", &self.manifest.metadata.tools.len())
            .finish()
    }
}

impl PackAfb {
    /// Reads and fully validates a pack `.afb`.
    ///
    /// The codec has already enforced every container bound by the time
    /// this sees the bytes. What is added here is the pack's own contract:
    /// the manifest is held to exactly the rules a bundled pack is held
    /// to, every declared asset is present, and no member is present that
    /// the manifest did not declare.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let afb = Afb::from_bytes(bytes)
            .map_err(|error| anyhow::anyhow!("this is not a readable .afb: {error}"))?;
        Self::from_afb(&afb)
    }

    /// The same, for a caller that has already parsed the container.
    pub fn from_afb(afb: &Afb) -> Result<Self> {
        let mut assets: BTreeMap<String, Vec<u8>> = afb
            .source
            .iter()
            .filter_map(|(path, bytes)| {
                path.strip_prefix(SOURCE_PREFIX)
                    .map(|relative| (relative.to_owned(), bytes.clone()))
            })
            .collect();
        let manifest_bytes = assets
            .get("manifest.json")
            .context("this .afb carries no pack manifest, so it is not a pack")?
            .clone();
        let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
            .context("the pack manifest is not valid JSON")?;
        validate_manifest(&manifest.name.clone(), &manifest)?;
        ensure!(
            afb.manifest.package.name == manifest.name,
            "the package says it is {:?} and the pack manifest says {:?}",
            afb.manifest.package.name,
            manifest.name
        );
        ensure!(
            afb.manifest.package.version == manifest.version,
            "the package says version {:?} and the pack manifest says {:?}",
            afb.manifest.package.version,
            manifest.version
        );

        for path in &manifest.metadata.assets {
            ensure!(
                assets.contains_key(path),
                "the pack declares {path:?} and does not carry it"
            );
        }
        // The manifest is the whole description of a pack. A member it
        // does not declare would be invisible to every check the pack
        // system runs and to the pack's own digest, so it is a refusal
        // rather than something to ignore.
        let declared = declared_paths(&manifest);
        for path in assets.keys() {
            ensure!(
                declared.iter().any(|known| known == path),
                "the pack carries {path:?}, which its manifest does not declare"
            );
        }
        assets.insert("manifest.json".to_owned(), manifest_bytes);
        Ok(Self {
            manifest,
            assets,
            digest: afb.digest,
        })
    }

    pub fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// The tools this pack ships.
    pub fn tools(&self) -> &[PackTool] {
        &self.manifest.metadata.tools
    }

    /// One tool's compiled module, by the name it was declared under.
    pub fn tool_module(&self, name: &str) -> Result<&[u8]> {
        let tool = self
            .manifest
            .metadata
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .with_context(|| format!("this pack ships no tool called {name:?}"))?;
        self.asset(&tool.module)
    }

    /// One asset's bytes. `manifest.json` is addressable like any other.
    pub fn asset(&self, path: &str) -> Result<&[u8]> {
        self.assets
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("this pack carries no asset {path:?}"))
    }

    /// The pack's identity: the digest this pack would have if it had been
    /// compiled into a binary instead of published.
    pub fn digest(&self) -> Result<String> {
        crate::pack::digest_declared_assets(&self.manifest, |path| self.asset(path))
    }

    /// SHA-256 of the exact `.afb` bytes this was read from. What a
    /// registry addresses the artifact by, and what a download is checked
    /// against before it is opened.
    pub fn artifact_digest(&self) -> String {
        afterburner_afb::digest::hex(&self.digest)
    }

    /// Writes the pack out as a directory, creating parents as needed.
    ///
    /// Paths were checked when the pack was read; the check is repeated
    /// because a caller must not be able to write outside the directory it
    /// named, whatever it managed to construct.
    pub fn write_to(&self, dir: &Path) -> Result<()> {
        for (path, bytes) in &self.assets {
            ensure!(
                crate::pack::is_distributable_asset_path(path),
                "refusing to write pack asset {path:?}"
            );
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&target, bytes)
                .with_context(|| format!("writing {}", target.display()))?;
        }
        Ok(())
    }
}

/// What a pack is published as: the namespace and version its `.afb`
/// carries, which is how a registry indexes and a client asks for it.
#[derive(Clone, Debug)]
pub struct PublishAs {
    pub namespace: String,
}

impl Default for PublishAs {
    fn default() -> Self {
        Self {
            namespace: DEFAULT_NAMESPACE.to_owned(),
        }
    }
}

/// Packs a pack directory into `.afb` bytes, returning them with the
/// artifact's own digest.
///
/// The manifest decides what travels: an asset it declares and the
/// directory does not have is an error, and a file the directory has and
/// the manifest does not declare is simply not packed. What ships is
/// exactly what the pack says it is, which is also what its digest covers.
///
/// Compiling a pack's tools is not this function's job. It packs what is
/// on disk, so a module a tool declares must already be built; `gents pack
/// build` is what compiles first and then calls this.
pub fn pack_dir(dir: &Path, publish_as: &PublishAs) -> Result<(Vec<u8>, String)> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    validate_manifest(&manifest.name.clone(), &manifest)?;

    let afb_manifest = afb_manifest_for(&manifest, publish_as)?;
    let mut builder = pack::Builder::new(afb_manifest, manifold_for(&manifest)?);
    builder = builder.source(
        format!("{SOURCE_PREFIX}manifest.json"),
        manifest_bytes.clone(),
    );
    for path in &manifest.metadata.assets {
        let source = dir.join(path);
        let bytes = std::fs::read(&source).with_context(|| {
            format!(
                "the pack declares {path:?} and {} is unreadable; a tool module has to be built \
                 before the pack is packed",
                source.display()
            )
        })?;
        builder = builder.source(format!("{SOURCE_PREFIX}{path}"), bytes);
    }
    let (bytes, digest) = builder
        .build()
        .map_err(|error| anyhow::anyhow!("building the pack .afb: {error}"))?;
    Ok((bytes, afterburner_afb::digest::hex(&digest)))
}

/// The `afb.toml` a pack is published under.
fn afb_manifest_for(manifest: &PackManifest, publish_as: &PublishAs) -> Result<Manifest> {
    let toml = format!(
        "[format]\nversion = \"{}\"\n\n\
         [package]\nname = \"{}\"\nnamespace = \"{}\"\nversion = \"{}\"\n\
         language = \"{PACK_LANGUAGE}\"\nentry = \"{SOURCE_PREFIX}manifest.json\"\n\
         description = {}\nkeywords = {}\n\n\
         [runtime]\nmin = \"{}\"\n",
        afterburner_afb::reader_format_version(),
        manifest.name,
        publish_as.namespace,
        manifest.version,
        serde_json::to_string(&manifest.description).context("encoding the description")?,
        serde_json::to_string(&manifest.metadata.tags).context("encoding the tags")?,
        afterburner_core::VERSION,
    );
    manifest::Manifest::parse(&toml)
        .map_err(|error| anyhow::anyhow!("building this pack's afb.toml: {error}"))
}

/// What the pack's tools may reach.
///
/// A pack that declares no manifold for a tool asks for nothing, which is
/// the right default for a pure transform. A pack that does declare one
/// gets it as written; an operator's ceiling narrows it at admission and
/// can never widen it, which is enforced where the tool runs, not here.
fn manifold_for(manifest: &PackManifest) -> Result<Manifold> {
    let mut requested = Manifold::default();
    for tool in &manifest.metadata.tools {
        let Some(declared) = &tool.manifold else {
            continue;
        };
        let parsed: Manifold = serde_json::from_value(declared.clone())
            .with_context(|| format!("the manifold declared by tool {:?} is not one", tool.name))?;
        // The package's manifold is the union of what its tools ask for,
        // because the container carries one. Each tool is still admitted
        // against its own declaration where it runs, so the union widens
        // nothing at the call.
        requested = union_manifold(requested, parsed);
    }
    Ok(requested)
}

/// The wider of two capability sets, field by field.
///
/// Written out rather than derived, so a capability added to `Manifold`
/// upstream fails this to compile instead of silently defaulting one way
/// or the other. Where two grants of the same shape differ, the union is
/// the concatenation of what each asked for, because a package's manifold
/// has to cover every tool it carries; each tool is still admitted against
/// its own declaration where it runs, so this widens nothing at the call.
fn union_manifold(left: Manifold, right: Manifold) -> Manifold {
    use afterburner_core::manifold::{EnvAccess, FsAccess, NetAccess};

    /// Two host allow-lists become one, and either side asking for "any
    /// host" makes the union any host.
    fn hosts(left: &Option<Vec<String>>, right: &Option<Vec<String>>) -> Option<Vec<String>> {
        match (left, right) {
            (None, _) | (_, None) => None,
            (Some(left), Some(right)) => {
                let mut merged = left.clone();
                merged.extend(right.iter().cloned());
                merged.sort();
                merged.dedup();
                Some(merged)
            }
        }
    }

    fn roots(left: &[std::path::PathBuf], right: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
        let mut merged = left.to_vec();
        merged.extend(right.iter().cloned());
        merged.sort();
        merged.dedup();
        merged
    }

    Manifold {
        fs: match (&left.fs, &right.fs) {
            (FsAccess::None, other) | (other, FsAccess::None) => other.clone(),
            // Read-write is the wider of the two, so a mixed pair takes
            // every root either side named at the wider level.
            (FsAccess::ReadWrite(a), FsAccess::ReadWrite(b))
            | (FsAccess::ReadWrite(a), FsAccess::ReadOnly(b))
            | (FsAccess::ReadOnly(a), FsAccess::ReadWrite(b)) => FsAccess::ReadWrite(roots(a, b)),
            (FsAccess::ReadOnly(a), FsAccess::ReadOnly(b)) => FsAccess::ReadOnly(roots(a, b)),
        },
        net: match (&left.net, &right.net) {
            (NetAccess::None, other) | (other, NetAccess::None) => other.clone(),
            (NetAccess::OutboundFull(a), NetAccess::OutboundFull(b))
            | (NetAccess::OutboundFull(a), NetAccess::OutboundHttp(b))
            | (NetAccess::OutboundHttp(a), NetAccess::OutboundFull(b)) => {
                NetAccess::OutboundFull(hosts(a, b))
            }
            (NetAccess::OutboundHttp(a), NetAccess::OutboundHttp(b)) => {
                NetAccess::OutboundHttp(hosts(a, b))
            }
        },
        env: match (&left.env, &right.env) {
            (EnvAccess::None, other) | (other, EnvAccess::None) => other.clone(),
            (EnvAccess::Full, _) | (_, EnvAccess::Full) => EnvAccess::Full,
            (EnvAccess::AllowList(a), EnvAccess::AllowList(b)) => {
                let mut merged = a.clone();
                merged.extend(b.iter().cloned());
                merged.sort();
                merged.dedup();
                EnvAccess::AllowList(merged)
            }
        },
        // A pack's tool is called, never a server: nothing in a pack has
        // a reason to bind a port, so this stays denied whatever either
        // side asked for, and a tool that wanted one is refused when the
        // pack is validated rather than quietly granted here.
        listen: afterburner_core::manifold::ListenAccess::None,
        crypto: left.crypto || right.crypto,
        child_process: left.child_process || right.child_process,
        allow_exit: left.allow_exit || right.allow_exit,
        http_timeout_ms: left.http_timeout_ms.max(right.http_timeout_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one of this build's own bundled packs out as a directory, so
    /// the tests work on a real pack rather than a fixture that could
    /// drift from what a pack actually looks like.
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

    #[test]
    fn a_packed_pack_reads_back_as_the_same_pack() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root, &PublishAs::default()).expect("packing");
        let packed = PackAfb::from_bytes(&bytes).expect("reading back");

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
    fn a_pack_has_the_same_identity_however_it_arrived() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root, &PublishAs::default()).expect("packing");
        let packed = PackAfb::from_bytes(&bytes).expect("reading");
        let bundled = crate::pack::resolve_pack("mailbox").expect("a bundled pack");
        assert_eq!(
            packed.digest().expect("packed digest"),
            bundled.digest,
            "a pack installed from a registry must be the same pack as the one compiled in"
        );
    }

    #[test]
    fn packing_twice_gives_the_same_bytes() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (first, first_digest) = pack_dir(&root, &PublishAs::default()).expect("first");
        let (second, second_digest) = pack_dir(&root, &PublishAs::default()).expect("second");
        assert_eq!(
            first, second,
            "a rebuild that changes nothing changes no bytes"
        );
        assert_eq!(first_digest, second_digest);
    }

    #[test]
    fn a_pack_is_an_afb_a_registry_can_index() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, digest) = pack_dir(&root, &PublishAs::default()).expect("packing");
        // The registry parses artifacts with the codec, not with anything
        // of ours, so this is the check that a pack really is publishable
        // without the registry learning a second format.
        let afb = Afb::from_bytes(&bytes).expect("the codec reads it");
        assert_eq!(afb.qualified_name(), "gents/mailbox");
        assert_eq!(afb.manifest.package.version, "1.0.0");
        assert_eq!(afterburner_afb::digest::hex(&afb.digest), digest);
        assert_eq!(
            PackAfb::from_afb(&afb)
                .expect("and it is a pack")
                .digest()
                .unwrap(),
            crate::pack::resolve_pack("mailbox").unwrap().digest
        );
    }

    #[test]
    fn an_afb_that_is_not_a_pack_is_refused() {
        let toml = format!(
            "[format]\nversion = \"{}\"\n\n[package]\nname = \"widget\"\nnamespace = \"acme\"\n\
             version = \"0.1.0\"\nlanguage = \"js\"\nentry = \"source/main.js\"\n\n\
             [runtime]\nmin = \"{}\"\n",
            afterburner_afb::reader_format_version(),
            afterburner_core::VERSION,
        );
        let manifest = manifest::Manifest::parse(&toml).expect("a manifest");
        let (bytes, _) = pack::Builder::new(manifest, Manifold::default())
            .source("source/main.js", b"module.exports = () => {};".to_vec())
            .build()
            .expect("building");
        let error = PackAfb::from_bytes(&bytes).expect_err("must be refused");
        assert!(format!("{error:#}").contains("not a pack"), "{error:#}");
    }

    #[test]
    fn a_member_the_manifest_does_not_declare_is_refused() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        std::fs::write(root.join("stowaway.md"), b"not declared").expect("write");
        let (bytes, _) = pack_dir(&root, &PublishAs::default()).expect("packing");
        // Packing ignores it, because the manifest is the description.
        assert!(PackAfb::from_bytes(&bytes)
            .expect("reading")
            .asset("stowaway.md")
            .is_err());

        // And an archive built to carry it anyway is refused on the way in.
        let manifest_bytes = std::fs::read(root.join("manifest.json")).expect("manifest");
        let manifest: PackManifest = serde_json::from_slice(&manifest_bytes).expect("parse");
        let mut builder = pack::Builder::new(
            afb_manifest_for(&manifest, &PublishAs::default()).expect("afb manifest"),
            Manifold::default(),
        )
        .source(format!("{SOURCE_PREFIX}manifest.json"), manifest_bytes)
        .source(
            format!("{SOURCE_PREFIX}stowaway.md"),
            b"not declared".to_vec(),
        );
        for path in &manifest.metadata.assets {
            builder = builder.source(
                format!("{SOURCE_PREFIX}{path}"),
                std::fs::read(root.join(path)).expect("asset"),
            );
        }
        let (smuggled, _) = builder.build().expect("building");
        let error = PackAfb::from_bytes(&smuggled).expect_err("must be refused");
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
        let error = pack_dir(&root, &PublishAs::default()).expect_err("must be refused");
        assert!(format!("{error:#}").contains(&dropped), "{error:#}");
    }

    #[test]
    fn writing_a_pack_out_reproduces_the_directory_it_came_from() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, digest) = pack_dir(&root, &PublishAs::default()).expect("packing");
        let packed = PackAfb::from_bytes(&bytes).expect("reading");
        let out = tempfile::tempdir().expect("tempdir");
        packed.write_to(out.path()).expect("writing out");
        let (again, again_digest) = pack_dir(out.path(), &PublishAs::default()).expect("repacking");
        assert_eq!(
            again, bytes,
            "a pack written to disk and packed again is the same pack"
        );
        assert_eq!(again_digest, digest);
    }

    #[test]
    fn a_tool_pack_carries_its_module_and_asks_for_what_the_tool_asks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("shipping_tools");
        std::fs::create_dir_all(root.join("tools")).expect("mkdir");
        std::fs::write(root.join("README.md"), b"# shipping tools").expect("write");
        std::fs::write(root.join("tools/format_check.wasm"), b"\0asm\x01\0\0\0").expect("write");
        let manifest = serde_json::json!({
            "manifest_version": 1,
            "name": "shipping_tools",
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["tools"],
            "kind": "tools",
            "assets": ["README.md", "tools/format_check.wasm"],
            "tools": [{
                "name": "format_check",
                "description": "Checks formatting and says what is wrong",
                "module": "tools/format_check.wasm",
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

        let (bytes, _) = pack_dir(&root, &PublishAs::default()).expect("packing a tools pack");
        let packed = PackAfb::from_bytes(&bytes).expect("reading it back");
        assert_eq!(packed.tools().len(), 1);
        assert_eq!(packed.tools()[0].name, "format_check");
        assert_eq!(
            packed.tool_module("format_check").expect("the module"),
            b"\0asm\x01\0\0\0",
            "a tool's compiled module travels inside the pack"
        );

        // The package asks the sandbox for what its tool asked for, and
        // never for a port.
        let afb = Afb::from_bytes(&bytes).expect("the codec reads it");
        assert!(matches!(
            afb.manifold.fs,
            afterburner_core::manifold::FsAccess::ReadOnly(_)
        ));
        assert!(matches!(
            afb.manifold.listen,
            afterburner_core::manifold::ListenAccess::None
        ));
    }

    #[test]
    fn a_tool_whose_module_is_not_a_declared_asset_is_refused() {
        let mut manifest: crate::pack::PackManifest = serde_json::from_value(serde_json::json!({
            "manifest_version": 1,
            "name": "shipping_tools",
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["tools"],
            "kind": "tools",
            "assets": ["README.md"],
            "tools": [{
                "name": "format_check",
                "description": "Checks formatting",
                "module": "tools/format_check.wasm",
                "input_schema": {"type": "object"}
            }]
        }))
        .expect("a manifest");
        let error = validate_manifest("shipping_tools", &manifest).expect_err("must be refused");
        assert!(format!("{error:#}").contains("not its module"), "{error:#}");

        manifest
            .metadata
            .assets
            .push("tools/format_check.wasm".to_owned());
        validate_manifest("shipping_tools", &manifest).expect("and accepted once declared");
    }
}
