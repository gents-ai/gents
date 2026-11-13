//! The core pack archives this build carries, each as `(coordinate, the
//! .pack bytes)`, in the order init installs them. A release carries exactly
//! the builds it was tested with; a build made without `make core-packs`
//! carries none.

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/archives.rs"));
}

pub fn archives() -> &'static [(&'static str, &'static [u8])] {
    embedded::ARCHIVES
}
