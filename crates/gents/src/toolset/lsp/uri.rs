use std::path::{Path, PathBuf};

/// Convert a filesystem path to a `file://` URI with percent-encoding.
pub fn path_to_file_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    let raw = abs.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if cfg!(windows) && !raw.starts_with('/') {
        uri.push('/');
    }
    uri.push_str(&encode_path(&raw));
    uri
}

/// Parse a `file://` URI into a path. Tolerates unencoded `#`/`?` from lax servers.
pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    if !uri.starts_with("file:") {
        return Err(format!("unsupported URI scheme: {uri}"));
    }
    if uri.contains('#') || uri.contains('?') {
        return Ok(PathBuf::from(lax_file_uri_to_path(uri)));
    }
    match strict_file_uri_to_path(uri) {
        Ok(path) => Ok(path),
        Err(_) => Ok(PathBuf::from(lax_file_uri_to_path(uri))),
    }
}

fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if is_unreserved_path_byte(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_unreserved_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':')
}

fn strip_file_scheme(uri: &str) -> Option<&str> {
    uri.strip_prefix("file://")
        .or_else(|| uri.strip_prefix("file:"))
}

fn strict_file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = strip_file_scheme(uri).ok_or_else(|| format!("unsupported URI scheme: {uri}"))?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest)?;
    Ok(windows_drive_path(decoded))
}

fn lax_file_uri_to_path(uri: &str) -> PathBuf {
    let rest = strip_file_scheme(uri).unwrap_or(uri);
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest).unwrap_or_else(|_| rest.to_string());
    windows_drive_path(decoded)
}

fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("invalid percent-encoding in file URI".into());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "invalid percent-encoding in file URI")?;
            let value =
                u8::from_str_radix(hex, 16).map_err(|_| "invalid percent-encoding in file URI")?;
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "file URI is not valid UTF-8".into())
}

fn windows_drive_path(decoded: String) -> PathBuf {
    if cfg!(windows)
        && decoded.starts_with('/')
        && decoded.len() >= 3
        && decoded.as_bytes()[1].is_ascii_alphabetic()
        && decoded.as_bytes()[2] == b':'
    {
        PathBuf::from(&decoded[1..])
    } else {
        PathBuf::from(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_spaces_and_hash() {
        let path = PathBuf::from("/tmp/my project/a#b.rs");
        let uri = path_to_file_uri(&path);
        assert!(uri.contains("%20"), "{uri}");
        assert!(uri.contains("%23"), "{uri}");
        assert_eq!(file_uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn lax_server_raw_hash_does_not_truncate() {
        let path = file_uri_to_path("file:///tmp/a#b.rs").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/a#b.rs"));
    }

    #[test]
    fn accepts_single_slash_file_uri() {
        let path = file_uri_to_path("file:/tmp/spaced%20name.rs").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/spaced name.rs"));
    }
}
