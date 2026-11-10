use std::io::Write;

use anyhow::Result;

use super::BundleFiles;

/// Fixed owner, mode and times so the same files always archive to the same
/// bytes, whatever host or checkout produced them.
pub(crate) fn tar_gz(files: &BundleFiles) -> Result<Vec<u8>> {
    let encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::best());
    let mut builder = tar::Builder::new(encoder);
    for (path, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_path(path)?;
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder.append(&header, bytes.as_slice())?;
    }
    let mut encoder = builder.into_inner()?;
    encoder.flush()?;
    Ok(encoder.finish()?)
}
