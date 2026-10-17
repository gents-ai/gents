mod audio;
mod permission;
mod wire;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};

use gents_protocol::client_media::{
    MediaCall as Call, MediaDevices, MediaEvent, MediaInputDevice, MediaMessage,
    MediaOperation as Operation, MediaReply, VERSION,
};

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--describe"] {
        serde_json::to_writer(
            std::io::stdout().lock(),
            &json!({
                "name": env!("CARGO_PKG_NAME"), "version": env!("CARGO_PKG_VERSION"),
                "protocolVersion": VERSION, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                "maxFrameBytes": wire::MAX_FRAME_BYTES, "maxPcmBytes": audio::MAX_PCM_BYTES,
                "maxSeconds": audio::MAX_SECONDS,
            }),
        )?;
        return Ok(());
    }
    ensure!(
        arguments == ["--stdio"],
        "Use --stdio to attach an editor, or --describe to inspect this artifact."
    );
    serve()
}

/// The audio owner never waits on stdout. A bounded writer queue isolates
/// stalled pipes; EOF, writer failure and backpressure drop every stream.
fn serve() -> Result<()> {
    let stopped = Arc::new(AtomicBool::new(false));
    let (incoming, calls) = mpsc::sync_channel::<Result<Call>>(8);
    let closed = stopped.clone();
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            match wire::read(&mut input) {
                Ok(Some(value)) => {
                    let call = serde_json::from_value(value).map_err(|_| anyhow::anyhow!("Invalid media command. Install matching extension and helper versions."));
                    if incoming.send(call).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = incoming.try_send(Err(error));
                    break;
                }
            }
        }
        closed.store(true, Ordering::Release);
    });
    let (outgoing, messages) = mpsc::sync_channel::<MediaMessage>(4);
    let closed = stopped.clone();
    std::thread::spawn(move || {
        let mut output = std::io::stdout().lock();
        for message in messages {
            if wire::write(&mut output, &message).is_err() {
                break;
            }
        }
        closed.store(true, Ordering::Release);
    });
    let emit = |message| {
        outgoing
            .try_send(message)
            .context("Media output is unavailable or stalled.")
    };
    let mut initialized = false;
    let mut capture: Option<(String, audio::Capture)> = None;
    let mut playback: Option<(String, audio::Playback)> = None;
    while !stopped.load(Ordering::Acquire) {
        if capture.as_ref().is_some_and(|(_, value)| value.ready()) {
            let (token, active) = capture.take().context("No recording is active.")?;
            emit(MediaMessage::Event(captured(token, active.finish())))?;
        }
        if let Some(result) = playback.as_ref().and_then(|(_, value)| value.completion()) {
            let (token, active) = playback.take().context("No playback is active.")?;
            drop(active);
            emit(MediaMessage::Event(match result {
                Ok(()) => MediaEvent::Played { token },
                Err(error) => MediaEvent::Failed {
                    token,
                    message: error.to_string(),
                },
            }))?;
        }
        let call = match calls.recv_timeout(Duration::from_millis(10)) {
            Ok(call) => call?,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let result = (|| -> Result<Value> {
            if let Operation::Hello { version } = call.operation {
                ensure!(!initialized && version == VERSION, "Incompatible media protocol. Install matching extension and media host versions.");
                initialized = true;
                return Ok(
                    json!({"version":VERSION, "maxPcmBytes":audio::MAX_PCM_BYTES, "maxSeconds":audio::MAX_SECONDS}),
                );
            }
            ensure!(
                initialized,
                "Initialize the media protocol before using a device."
            );
            match call.operation {
                Operation::Hello { .. } => {
                    anyhow::bail!("The media protocol is already initialized.")
                }
                Operation::Devices {} => Ok(serde_json::to_value(MediaDevices {
                    inputs: audio::input_devices()?
                        .into_iter()
                        .map(|(id, label)| MediaInputDevice { id, label })
                        .collect(),
                    output_sample_rate: audio::output_sample_rate().ok(),
                })?),
                Operation::Capture { token, device } => {
                    valid_token(&token)?;
                    ensure!(
                        capture.is_none() && playback.is_none(),
                        "Stop the active recording or playback first."
                    );
                    let active = audio::Capture::start(device.as_deref(), &stopped)?;
                    let rate = active.sample_rate;
                    capture = Some((token, active));
                    Ok(json!({"sampleRate":rate}))
                }
                Operation::Finish { token } => {
                    ensure!(
                        capture.as_ref().is_some_and(|(active, _)| active == &token),
                        "This recording is no longer active."
                    );
                    let (_, active) = capture.take().context("No recording is active.")?;
                    emit(MediaMessage::Event(captured(token, active.finish())))?;
                    Ok(Value::Null)
                }
                Operation::Play {
                    token,
                    sample_rate,
                    pcm,
                } => {
                    valid_token(&token)?;
                    ensure!(
                        capture.is_none() && playback.is_none(),
                        "Stop the active recording or playback first."
                    );
                    let samples = gents_protocol::client_media::MediaPcmClip { pcm, sample_rate }
                        .samples()?;
                    playback = Some((
                        token,
                        audio::Playback::start(sample_rate, samples, &stopped)?,
                    ));
                    Ok(Value::Null)
                }
                Operation::Stop {} => {
                    capture = None;
                    playback = None;
                    Ok(Value::Null)
                }
            }
        })();
        emit(MediaMessage::Reply(match result {
            Ok(value) => MediaReply {
                id: call.id,
                result: value,
                error: None,
            },
            Err(error) => MediaReply {
                id: call.id,
                result: Value::Null,
                error: Some(error.to_string()),
            },
        }))?;
    }
    drop(capture);
    drop(playback);
    Ok(())
}

fn valid_token(token: &str) -> Result<()> {
    ensure!(
        !token.is_empty()
            && token.len() <= 128
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "Invalid media operation identity."
    );
    Ok(())
}

fn captured(token: String, result: Result<(u32, Vec<i16>)>) -> MediaEvent {
    match result {
        Ok((sample_rate, samples)) => {
            let bytes: Vec<u8> = samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            MediaEvent::Captured {
                token,
                sample_rate,
                pcm: base64::engine::general_purpose::STANDARD.encode(bytes),
            }
        }
        Err(error) => MediaEvent::Failed {
            token,
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_rejects_malformed_oversized_and_unaligned_audio() {
        for value in ["", "!", "AA=="] {
            assert!(gents_protocol::client_media::MediaPcmClip {
                pcm: value.into(),
                sample_rate: 48_000
            }
            .samples()
            .is_err());
        }
        assert!(gents_protocol::client_media::MediaPcmClip {
            pcm: "A".repeat(audio::MAX_PCM_BYTES.div_ceil(3) * 4 + 1),
            sample_rate: 48_000
        }
        .samples()
        .is_err());
        assert_eq!(
            gents_protocol::client_media::MediaPcmClip {
                pcm: "AID/fw==".into(),
                sample_rate: 48_000
            }
            .samples()
            .unwrap(),
            [i16::MIN, i16::MAX]
        );
    }

    #[test]
    fn commands_reject_unknown_fields_and_decode_optional_devices() {
        assert!(serde_json::from_value::<Call>(
            json!({"id":1,"operation":{"command":"capture","token":"test"}})
        )
        .is_ok());
        assert!(serde_json::from_value::<Call>(
            json!({"id":1,"operation":{"command":"stop","pcm":"secret"}})
        )
        .is_err());
        assert!(serde_json::from_value::<Call>(
            json!({"id":1,"operation":{"command":"stop"},"extra":true})
        )
        .is_err());
    }
}
