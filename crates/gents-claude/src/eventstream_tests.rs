//! Event-stream codec tests: encoded frames decode back in any split, and a
//! corrupt, oversize or truncated stream fails with the matching typed error.

use proptest::prelude::*;

use super::*;

fn sample() -> Frame {
    Frame {
        headers: vec![
            (":message-type".into(), HeaderValue::String("event".into())),
            (":event-type".into(), HeaderValue::String("chunk".into())),
            ("flag".into(), HeaderValue::Bool(true)),
            ("off".into(), HeaderValue::Bool(false)),
            ("byte".into(), HeaderValue::Byte(-7)),
            ("short".into(), HeaderValue::Short(-1234)),
            ("int".into(), HeaderValue::Int(70_000)),
            ("long".into(), HeaderValue::Long(-9_000_000_000)),
            ("bytes".into(), HeaderValue::Bytes(vec![0, 1, 255])),
            ("time".into(), HeaderValue::Timestamp(1_793_448_000_000)),
            ("id".into(), HeaderValue::Uuid([7; 16])),
        ],
        payload: br#"{"bytes":"e30="}"#.to_vec(),
    }
}

fn decode_all(bytes: &[u8]) -> Result<Vec<Frame>, EventStreamError> {
    let mut decoder = Decoder::new();
    let frames = decoder.push(bytes)?;
    decoder.finish()?;
    Ok(frames)
}

#[test]
fn a_frame_with_every_header_type_round_trips() {
    let frame = sample();
    assert_eq!(decode_all(&encode(&frame)).unwrap(), vec![frame.clone()]);
    assert_eq!(frame.string_header(":event-type"), Some("chunk"));
    assert_eq!(frame.string_header("flag"), None);
    assert_eq!(frame.string_header("missing"), None);
}

#[test]
fn an_empty_frame_is_the_framing_alone() {
    let bytes = encode(&Frame::default());
    assert_eq!(bytes.len(), FRAMING_BYTES);
    assert_eq!(decode_all(&bytes).unwrap(), vec![Frame::default()]);
}

#[test]
fn the_stream_splits_at_every_byte_boundary() {
    let frames = [sample(), Frame::default(), sample()];
    let bytes: Vec<u8> = frames.iter().flat_map(encode).collect();
    for cut in 0..=bytes.len() {
        let mut decoder = Decoder::new();
        let mut got = decoder.push(&bytes[..cut]).unwrap();
        got.extend(decoder.push(&bytes[cut..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(got, frames, "split at {cut}");
    }
    let mut decoder = Decoder::new();
    let mut got = Vec::new();
    for byte in &bytes {
        got.extend(decoder.push(std::slice::from_ref(byte)).unwrap());
    }
    assert_eq!(got, frames);
}

fn header_strategy() -> impl Strategy<Value = (String, HeaderValue)> {
    (
        "[a-z:-]{1,20}",
        prop_oneof![
            any::<bool>().prop_map(HeaderValue::Bool),
            any::<i8>().prop_map(HeaderValue::Byte),
            any::<i16>().prop_map(HeaderValue::Short),
            any::<i32>().prop_map(HeaderValue::Int),
            any::<i64>().prop_map(HeaderValue::Long),
            proptest::collection::vec(any::<u8>(), 0..40).prop_map(HeaderValue::Bytes),
            "\\PC{0,30}".prop_map(HeaderValue::String),
            any::<i64>().prop_map(HeaderValue::Timestamp),
            any::<[u8; 16]>().prop_map(HeaderValue::Uuid),
        ],
    )
}

proptest! {
    #[test]
    fn encoded_frames_decode_back_in_any_split(
        frames in proptest::collection::vec(
            (
                proptest::collection::vec(header_strategy(), 0..6),
                proptest::collection::vec(any::<u8>(), 0..300),
            ),
            0..5,
        ),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..8),
    ) {
        let frames: Vec<Frame> = frames
            .into_iter()
            .map(|(headers, payload)| Frame { headers, payload })
            .collect();
        let bytes: Vec<u8> = frames.iter().flat_map(encode).collect();
        let mut points: Vec<usize> = cuts.iter().map(|cut| cut.index(bytes.len() + 1)).collect();
        points.sort_unstable();
        let mut decoder = Decoder::new();
        let mut got = Vec::new();
        let mut from = 0;
        for point in points.into_iter().chain([bytes.len()]) {
            got.extend(decoder.push(&bytes[from..point]).unwrap());
            from = point;
        }
        decoder.finish().unwrap();
        prop_assert_eq!(got, frames);
    }

    #[test]
    fn a_flipped_byte_never_decodes_to_the_original(
        payload in proptest::collection::vec(any::<u8>(), 1..200),
        at in any::<prop::sample::Index>(),
        flip in 1u8..=255,
    ) {
        let frame = Frame { headers: vec![], payload };
        let mut bytes = encode(&frame);
        let at = at.index(bytes.len());
        bytes[at] ^= flip;
        if let Ok(frames) = decode_all(&bytes) {
            prop_assert_ne!(frames, vec![frame]);
        }
    }
}

#[test]
fn a_damaged_prelude_and_a_damaged_message_fail_with_their_own_errors() {
    let bytes = encode(&sample());
    let mut prelude = bytes.clone();
    prelude[5] ^= 1;
    assert_eq!(decode_all(&prelude), Err(EventStreamError::PreludeChecksum));
    let mut payload = bytes.clone();
    let middle = payload.len() - 6;
    payload[middle] ^= 1;
    assert_eq!(decode_all(&payload), Err(EventStreamError::MessageChecksum));
    let mut trailer = bytes;
    let last = trailer.len() - 1;
    trailer[last] ^= 1;
    assert_eq!(decode_all(&trailer), Err(EventStreamError::MessageChecksum));
}

fn prelude(total: u32, headers: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&headers.to_be_bytes());
    out.extend_from_slice(&crc(&out).to_be_bytes());
    out
}

#[test]
fn an_oversize_or_undersize_frame_fails_before_it_is_buffered() {
    for total in [
        0u32,
        15,
        u32::try_from(MAX_FRAME_BYTES).unwrap() + 1,
        u32::MAX,
    ] {
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.push(&prelude(total, 0)),
            Err(EventStreamError::FrameLength { announced: total })
        );
        assert!(decoder.buffer.is_empty(), "{total}");
    }
    assert_eq!(
        Decoder::new().push(&prelude(100, 200)),
        Err(EventStreamError::HeadersLength {
            announced: 200,
            total: 100
        })
    );
    let over = u32::try_from(MAX_HEADER_BYTES).unwrap() + 1;
    assert_eq!(
        Decoder::new().push(&prelude(over + 16, over)),
        Err(EventStreamError::HeadersLength {
            announced: over,
            total: over + 16
        })
    );
}

#[test]
fn memory_is_bounded_by_the_announced_frame() {
    let big = Frame {
        headers: vec![],
        payload: vec![9; 100_000],
    };
    let bytes = encode(&big);
    let mut decoder = Decoder::new();
    let mut peak = 0;
    for chunk in bytes.chunks(777) {
        decoder.push(chunk).unwrap();
        peak = peak.max(decoder.buffer.len());
    }
    assert!(peak <= bytes.len());
    assert!(decoder.buffer.is_empty());
}

#[test]
fn a_stream_cut_inside_a_frame_is_truncated_and_a_failed_decoder_stays_failed() {
    let bytes = encode(&sample());
    let mut decoder = Decoder::new();
    assert!(decoder.push(&bytes[..bytes.len() - 3]).unwrap().is_empty());
    assert_eq!(decoder.finish(), Err(EventStreamError::Truncated));

    let mut broken = bytes.clone();
    broken[2] ^= 1;
    let mut decoder = Decoder::new();
    let error = decoder.push(&broken).unwrap_err();
    assert_eq!(decoder.push(&bytes), Err(error.clone()));
    assert_eq!(decoder.finish(), Err(error));
}

#[test]
fn an_unknown_header_type_and_a_cut_header_are_malformed() {
    // A header block whose single header has type tag 10, wrapped in a valid frame.
    let wrap = |headers: &[u8]| {
        let total = u32::try_from(FRAMING_BYTES + headers.len()).unwrap();
        let mut out = prelude(total, u32::try_from(headers.len()).unwrap());
        out.extend_from_slice(headers);
        out.extend_from_slice(&crc(&out).to_be_bytes());
        out
    };
    assert_eq!(
        decode_all(&wrap(&[1, b'a', 10])),
        Err(EventStreamError::MalformedHeader)
    );
    assert_eq!(
        decode_all(&wrap(&[1, b'a', 7, 0, 9, b'x'])),
        Err(EventStreamError::MalformedHeader)
    );
    assert_eq!(
        decode_all(&wrap(&[1, b'a', 7, 0, 1, 0xff])),
        Err(EventStreamError::MalformedHeader)
    );
}
