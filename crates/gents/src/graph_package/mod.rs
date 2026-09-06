mod catalog;
mod install;

pub(crate) use catalog::{digest_assets, graph_manifest_from_pack};

pub use catalog::{
    graph_package_catalog, load_bundled_graph_package, BundledGraphPackage,
    GraphPackageCatalogEntry, GraphPackageManifest, PackageCapabilityTemplate,
    PackageExternalDependency, PackageRoleDeclaration,
};
pub use install::{
    bundled_graph_id, default_bundled_graph_package_install_bindings,
    install_bundled_graph_package, prepare_bundled_graph_package_install,
    GraphPackageInstallBindings, GraphPackageInstallReceipt,
};
