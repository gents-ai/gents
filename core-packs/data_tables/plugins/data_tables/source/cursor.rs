//! Cursors: where the next page of a result starts, and a check that the
//! request and the data are still the ones the cursor came from.
//!
//! A cursor is `dt1.<base64url of "<fingerprint hex>:<rows already returned>">.<checksum>`.
//! The fingerprint covers the mode, the SQL, the options that change a result,
//! and the name and content fingerprint of every table the query opened. It is
//! opaque to the caller, who passes it back unchanged.
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::Res;
use crate::table::Fnv;

const PREFIX: &str = "dt1";

fn checksum(payload: &str) -> u64 {
    let mut h = Fnv::default();
    h.write(payload.as_bytes());
    h.0
}

/// The cursor for the page that starts after `offset` rows of the request `fingerprint`.
pub fn encode(fingerprint: u64, offset: u64) -> String {
    let payload = format!("{fingerprint:016x}:{offset}");
    format!(
        "{PREFIX}.{}.{:016x}",
        URL_SAFE_NO_PAD.encode(&payload),
        checksum(&payload)
    )
}

/// The fingerprint and row offset inside a cursor, once its checksum is verified.
pub fn parts(cursor: &str) -> Res<(u64, u64)> {
    let invalid = || {
        "the cursor is not valid; pass the next.cursor of the previous call back unchanged"
            .to_string()
    };
    let mut parts = cursor.split('.');
    let (Some(PREFIX), Some(body), Some(sum), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    let payload = URL_SAFE_NO_PAD
        .decode(body)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(invalid)?;
    if format!("{:016x}", checksum(&payload)) != sum {
        return Err(invalid());
    }
    let (fp, offset) = payload.split_once(':').ok_or_else(invalid)?;
    let offset: u64 = offset.parse().map_err(|_| invalid())?;
    let fp = u64::from_str_radix(fp, 16).map_err(|_| invalid())?;
    Ok((fp, offset))
}

/// Why a cursor's fingerprint does not match the request.
pub fn mismatch() -> String {
    "the cursor belongs to a different query or the data changed; start again without a cursor"
        .into()
}

/// The row offset a cursor names, once it is checked against the request's `fingerprint`.
pub fn decode(cursor: &str, fingerprint: u64) -> Res<u64> {
    let (fp, offset) = parts(cursor)?;
    if fp != fingerprint {
        return Err(mismatch());
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_cursor_names_its_offset() {
        assert_eq!(decode(&encode(7, 1234), 7), Ok(1234));
        assert_eq!(decode(&encode(u64::MAX, 0), u64::MAX), Ok(0));
    }

    #[test]
    fn a_cursor_from_another_request_is_refused_with_the_reason() {
        let err = decode(&encode(7, 10), 8).unwrap_err();
        assert!(err.contains("different query or the data changed"), "{err}");
    }

    #[test]
    fn damaged_or_forged_cursors_are_refused() {
        let good = encode(7, 10);
        let mut tampered = good.clone();
        tampered.replace_range(4..5, if &good[4..5] == "A" { "B" } else { "A" });
        for bad in [
            "",
            "dt1",
            "dt1..",
            "x.y.z",
            &tampered,
            &format!("{good}.extra"),
            "dt2.MDAwMDAwMDAwMDAwMDAwNzoxMA.0000000000000000",
        ] {
            let err = decode(bad, 7).unwrap_err();
            assert!(err.contains("cursor"), "{bad}: {err}");
        }
    }

    proptest! {
        #[test]
        fn encode_decode_round_trip(fp in any::<u64>(), offset in any::<u64>()) {
            prop_assert_eq!(decode(&encode(fp, offset), fp), Ok(offset));
        }

        #[test]
        fn garbage_never_panics(s in ".{0,60}") {
            let _ = decode(&s, 1);
        }
    }
}
