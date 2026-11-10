//! Tests for Bun-compatible gzip.

use std::io::Read;

use flate2::read::GzDecoder;

use super::compress::gzip;

fn inflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    GzDecoder::new(data).read_to_end(&mut out).unwrap();
    out
}

fn sample(len: usize) -> Vec<u8> {
    // Mixed content so the compressor does real work.
    let mut x = 0x9e37_79b9_u32;
    (0..len)
        .map(|i| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if i % 3 == 0 {
                (x >> 24) as u8
            } else {
                b"claude"[i % 6]
            }
        })
        .collect()
}

#[test]
fn round_trips_empty_small_and_4k() {
    for len in [0, 1, 17, 4096] {
        let input = sample(len);
        assert_eq!(inflate(&gzip(&input).unwrap()), input, "len {len}");
    }
}

#[test]
fn libdeflate_path_header_matches_bun() {
    let out = gzip(&sample(4096)).unwrap();
    assert_eq!(out[..10], [0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);
}

#[test]
fn zlib_ng_path_round_trips_and_is_deterministic() {
    let input = sample(600 * 1024);
    let a = gzip(&input).unwrap();
    assert_eq!(inflate(&a), input);
    assert_eq!(a, gzip(&input).unwrap());
    assert_eq!(a[..4], [0x1f, 0x8b, 0x08, 0x00]);
    #[cfg(target_os = "linux")]
    assert_eq!(a[9], 3, "zlib-ng writes the Unix OS byte");
}

#[test]
fn libdeflate_path_is_deterministic() {
    let input = sample(100 * 1024);
    assert_eq!(gzip(&input).unwrap(), gzip(&input).unwrap());
}

#[test]
fn switches_encoder_at_the_shared_buffer_bound() {
    // Incompressible input crosses the 512 KiB bound just under 512 KiB.
    let small = gzip(&sample(300 * 1024)).unwrap();
    let large = gzip(&sample(520 * 1024)).unwrap();
    assert_eq!(small[9], 0xff);
    assert_ne!(large[9], 0xff);
}
