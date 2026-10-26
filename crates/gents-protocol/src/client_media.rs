use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PCM_BYTES: usize = 3 * 1024 * 1024;
pub const MAX_SECONDS: u32 = 30;

#[derive(Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MediaCall {
    pub id: u32,
    pub operation: MediaOperation,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "command", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum MediaOperation {
    Hello {
        version: u32,
    },
    Devices {},
    Capture {
        token: String,
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        device: Option<String>,
    },
    Finish {
        token: String,
    },
    Play {
        token: String,
        #[serde(rename = "sampleRate")]
        sample_rate: u32,
        pcm: String,
    },
    Stop {},
}

/// Transient mono signed 16-bit little-endian PCM. Audio stays outside the
/// durable session and is discarded when its view or capture epoch retires.
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaPcmClip {
    pub sample_rate: u32,
    pub pcm: String,
}

impl std::fmt::Debug for MediaPcmClip {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaPcmClip")
            .field("sample_rate", &self.sample_rate)
            .field("encoded_bytes", &self.pcm.len())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MediaPcmError {
    #[error("Speech audio exceeds the supported format or clip limit.")]
    OutOfBounds,
    #[error("Speech audio is not valid base64 PCM.")]
    InvalidBase64,
    #[error("Speech audio must contain bounded mono signed 16-bit PCM.")]
    InvalidPcm,
}

impl MediaPcmClip {
    fn bytes(&self) -> Result<Vec<u8>, MediaPcmError> {
        use base64::Engine;
        if !((8_000..=192_000).contains(&self.sample_rate)
            && self.pcm.len() <= MAX_PCM_BYTES.div_ceil(3) * 4)
        {
            return Err(MediaPcmError::OutOfBounds);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.pcm)
            .map_err(|_| MediaPcmError::InvalidBase64)?;
        if !(!bytes.is_empty()
            && bytes.len().is_multiple_of(2)
            && bytes.len() <= MAX_PCM_BYTES
            && bytes.len() / 2 <= self.sample_rate as usize * MAX_SECONDS as usize)
        {
            return Err(MediaPcmError::InvalidPcm);
        }
        Ok(bytes)
    }

    pub fn validate(&self) -> Result<(), MediaPcmError> {
        self.bytes().map(drop)
    }

    pub fn samples(&self) -> Result<Vec<i16>, MediaPcmError> {
        let bytes = self.bytes()?;
        let (pairs, _) = bytes.as_chunks::<2>();
        Ok(pairs.iter().map(|pair| i16::from_le_bytes(*pair)).collect())
    }
}

#[derive(Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum MediaEvent {
    Captured {
        token: String,
        #[serde(rename = "sampleRate")]
        sample_rate: u32,
        pcm: String,
    },
    Played {
        token: String,
    },
    Failed {
        token: String,
        message: String,
    },
}

#[derive(Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct MediaReply {
    pub id: u32,
    pub result: Value,
    pub error: Option<String>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum MediaMessage {
    Reply(MediaReply),
    Event(MediaEvent),
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MediaDevices {
    pub inputs: Vec<MediaInputDevice>,
    pub output_sample_rate: Option<u32>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct MediaInputDevice {
    pub id: String,
    pub label: String,
}
