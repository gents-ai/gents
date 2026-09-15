mod catalog;
mod install;
mod run_adapter;

#[cfg(test)]
pub(crate) use catalog::load_package;
pub(crate) use catalog::{digest_assets, load_resolved_graph_package_with_environment};
pub(crate) use install::{
    bundled_graph_package_install_bindings_for_owner, install_loaded_graph_package,
};

pub use catalog::{
    graph_package_catalog, load_bundled_graph_package, load_resolved_graph_package,
    BundledGraphPackage, GraphPackageCatalogEntry, GraphPackageManifest, PackageCapabilityTemplate,
    PackageExternalDependency,
};
pub use install::{
    default_bundled_graph_package_install_bindings, install_bundled_graph_package,
    install_bundled_graph_package_for_graph, load_installed_package_plan,
    prepare_bundled_graph_package_install, prepare_bundled_graph_package_install_for_graph,
    GraphPackageInstallBindings, GraphPackageInstallReceipt,
};
pub use run_adapter::{prepare_code_review_run, PreparedGraphRun};
