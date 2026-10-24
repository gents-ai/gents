use std::io::{Read, Write};

use serde_json::Value;

pub use gents_protocol::client_media::MAX_FRAME_BYTES;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Incomplete media frame header.")]
    IncompleteHeader(#[source] std::io::Error),
    #[error("Invalid media frame length.")]
    InvalidLength,
    #[error("Incomplete media frame body.")]
    IncompleteBody(#[source] std::io::Error),
    #[error("Invalid media frame JSON.")]
    InvalidJson(#[source] serde_json::Error),
    #[error(transparent)]
    Encode(serde_json::Error),
    #[error("Media response exceeds the frame limit.")]
    ResponseTooLarge,
}

type Result<T, E = FrameError> = std::result::Result<T, E>;

pub fn read(input: &mut impl Read) -> Result<Option<Value>> {
    let mut header = [0; 4];
    if input.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    input
        .read_exact(&mut header[1..])
        .map_err(FrameError::IncompleteHeader)?;
    let length = u32::from_be_bytes(header) as usize;
    if !(1..=MAX_FRAME_BYTES).contains(&length) {
        return Err(FrameError::InvalidLength);
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .map_err(FrameError::IncompleteBody)?;
    Ok(Some(
        serde_json::from_slice(&bytes).map_err(FrameError::InvalidJson)?,
    ))
}

pub fn write(output: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(FrameError::Encode)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(FrameError::ResponseTooLarge);
    }
    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_is_distinct_from_a_truncated_frame() {
        assert_eq!(read(&mut &[][..]).unwrap(), None);
        for bytes in [&[0][..], &[0, 0, 0, 2, b'{'][..]] {
            assert!(read(&mut &*bytes).is_err());
        }
    }

    #[test]
    fn oversized_and_empty_frames_are_rejected_before_allocation() {
        for length in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
            assert!(read(&mut &length.to_be_bytes()[..]).is_err());
        }
    }

    #[test]
    fn messages_roundtrip_without_consuming_the_next_frame() {
        let mut bytes = Vec::new();
        write(&mut bytes, &serde_json::json!({"id":1})).unwrap();
        write(&mut bytes, &serde_json::json!({"id":2})).unwrap();
        let mut input = bytes.as_slice();
        assert_eq!(read(&mut input).unwrap(), Some(serde_json::json!({"id":1})));
        assert_eq!(read(&mut input).unwrap(), Some(serde_json::json!({"id":2})));
        assert_eq!(read(&mut input).unwrap(), None);
    }
}
