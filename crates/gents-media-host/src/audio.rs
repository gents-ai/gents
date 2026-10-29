use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};

pub use gents_protocol::client_media::{MAX_PCM_BYTES, MAX_SECONDS};

pub fn input_devices() -> Result<Vec<(String, String)>> {
    cpal::default_host()
        .input_devices()?
        .map(|device| Ok((device.id()?.to_string(), device.to_string())))
        .collect()
}

fn input_device(id: Option<&str>) -> Result<Device> {
    let host = cpal::default_host();
    match id {
        None => host.default_input_device().context("No microphone is available. Connect one and choose it in Gents voice settings."),
        Some(id) => host.input_devices()?.find(|device| device.id().is_ok_and(|value| value.to_string() == id))
            .context("The selected microphone is unavailable. Choose an available microphone in Gents voice settings."),
    }
}

pub struct Capture {
    stream: Stream,
    samples: Arc<Mutex<Vec<i16>>>,
    failed: Arc<AtomicBool>,
    full: Arc<AtomicBool>,
    deadline: Instant,
    pub sample_rate: u32,
}

impl Capture {
    /// Capture begins only after an explicit editor start. The callback writes
    /// to a preallocated bounded buffer; neither audio nor device errors are logged.
    pub fn start(device_id: Option<&str>, stopped: &AtomicBool) -> Result<Self> {
        crate::permission::authorize(stopped)?;
        ensure!(
            !stopped.load(Ordering::Acquire),
            "Microphone capture cancelled."
        );
        let device = input_device(device_id)?;
        let supported = device.default_input_config()?;
        let config = supported.config();
        ensure!(
            (8_000..=192_000).contains(&config.sample_rate) && (1..=32).contains(&config.channels),
            "The microphone's default audio format is unsupported."
        );
        let limit = (config.sample_rate as usize * MAX_SECONDS as usize).min(MAX_PCM_BYTES / 2);
        let samples = Arc::new(Mutex::new(Vec::with_capacity(limit)));
        let failed = Arc::new(AtomicBool::new(false));
        let full = Arc::new(AtomicBool::new(false));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => capture_stream::<f32>(
                &device,
                config,
                limit,
                samples.clone(),
                failed.clone(),
                full.clone(),
            )?,
            SampleFormat::F64 => capture_stream::<f64>(
                &device,
                config,
                limit,
                samples.clone(),
                failed.clone(),
                full.clone(),
            )?,
            SampleFormat::I16 => capture_stream::<i16>(
                &device,
                config,
                limit,
                samples.clone(),
                failed.clone(),
                full.clone(),
            )?,
            SampleFormat::I32 => capture_stream::<i32>(
                &device,
                config,
                limit,
                samples.clone(),
                failed.clone(),
                full.clone(),
            )?,
            SampleFormat::U16 => capture_stream::<u16>(
                &device,
                config,
                limit,
                samples.clone(),
                failed.clone(),
                full.clone(),
            )?,
            _ => bail!("The microphone's default sample format is unsupported."),
        };
        ensure!(
            !stopped.load(Ordering::Acquire),
            "Microphone capture cancelled."
        );
        stream.play().context(
            "Cannot start the microphone. Check microphone permission in system settings.",
        )?;
        Ok(Self {
            stream,
            samples,
            failed,
            full,
            deadline: Instant::now() + Duration::from_secs(u64::from(MAX_SECONDS)),
            sample_rate: config.sample_rate,
        })
    }

    pub fn ready(&self) -> bool {
        self.failed.load(Ordering::Acquire)
            || self.full.load(Ordering::Acquire)
            || Instant::now() >= self.deadline
    }

    pub fn finish(self) -> Result<(u32, Vec<i16>)> {
        drop(self.stream);
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "Microphone capture stopped. Check its connection and system microphone permission."
        );
        let samples = std::mem::take(
            &mut *self
                .samples
                .lock()
                .map_err(|_| anyhow::anyhow!("Microphone capture failed."))?,
        );
        ensure!(
            !samples.is_empty(),
            "The microphone returned no audio. Check its connection and microphone permission."
        );
        Ok((self.sample_rate, samples))
    }
}

/// CPAL receives a finite initialization wait so supported backends need not
/// wait indefinitely for the device; CPAL does not guarantee every backend honors it.
fn capture_stream<T>(
    device: &Device,
    config: StreamConfig,
    limit: usize,
    samples: Arc<Mutex<Vec<i16>>>,
    failed: Arc<AtomicBool>,
    full: Arc<AtomicBool>,
) -> Result<Stream>
where
    T: SizedSample,
    i16: FromSample<T>,
{
    let error = failed.clone();
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            let Ok(mut buffer) = samples.try_lock() else {
                failed.store(true, Ordering::Release);
                return;
            };
            if append_samples(data, config.channels, limit, &mut buffer) {
                full.store(true, Ordering::Release);
            }
        },
        move |_| {
            error.store(true, Ordering::Release);
        },
        Some(Duration::from_secs(5)),
    )?)
}

fn append_samples<T>(data: &[T], channels: u16, limit: usize, buffer: &mut Vec<i16>) -> bool
where
    T: SizedSample,
    i16: FromSample<T>,
{
    for frame in data.chunks_exact(usize::from(channels)) {
        if buffer.len() == limit {
            return true;
        }
        let sum: i64 = frame
            .iter()
            .map(|value| i64::from(i16::from_sample(*value)))
            .sum();
        buffer.push((sum / i64::from(channels)) as i16);
    }
    buffer.len() == limit
}

pub fn output_sample_rate() -> Result<u32> {
    Ok(cpal::default_host()
        .default_output_device()
        .context("No playback device is available.")?
        .default_output_config()?
        .sample_rate())
}

pub struct Playback {
    _stream: Stream,
    failed: Arc<AtomicBool>,
    finished_at: Arc<Mutex<Option<Instant>>>,
    deadline: Instant,
}

impl Playback {
    /// The completion deadline bounds waiting for a device that never reports
    /// the end of a clip after the protocol's maximum clip duration.
    pub fn start(sample_rate: u32, samples: Vec<i16>, stopped: &AtomicBool) -> Result<Self> {
        ensure!(
            !stopped.load(Ordering::Acquire),
            "Speech playback cancelled."
        );
        ensure!(
            (8_000..=192_000).contains(&sample_rate),
            "Speech playback has an unsupported sample rate."
        );
        ensure!(
            !samples.is_empty()
                && samples.len() <= MAX_PCM_BYTES / 2
                && samples.len() <= sample_rate as usize * MAX_SECONDS as usize,
            "Speech playback exceeds the clip limit or contains no samples."
        );
        let device = cpal::default_host()
            .default_output_device()
            .context("No playback device is available.")?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        ensure!(
            config.sample_rate == sample_rate,
            "The playback device changed. Synthesize speech again at its current sample rate."
        );
        ensure!(
            (1..=32).contains(&config.channels),
            "The playback device's channel count is unsupported."
        );
        let failed = Arc::new(AtomicBool::new(false));
        let finished_at = Arc::new(Mutex::new(None));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => playback_stream::<f32>(
                &device,
                config,
                samples,
                failed.clone(),
                finished_at.clone(),
            )?,
            SampleFormat::F64 => playback_stream::<f64>(
                &device,
                config,
                samples,
                failed.clone(),
                finished_at.clone(),
            )?,
            SampleFormat::I16 => playback_stream::<i16>(
                &device,
                config,
                samples,
                failed.clone(),
                finished_at.clone(),
            )?,
            SampleFormat::I32 => playback_stream::<i32>(
                &device,
                config,
                samples,
                failed.clone(),
                finished_at.clone(),
            )?,
            SampleFormat::U16 => playback_stream::<u16>(
                &device,
                config,
                samples,
                failed.clone(),
                finished_at.clone(),
            )?,
            _ => bail!("The playback device's default sample format is unsupported."),
        };
        ensure!(
            !stopped.load(Ordering::Acquire),
            "Speech playback cancelled."
        );
        stream.play()?;
        Ok(Self {
            _stream: stream,
            failed,
            finished_at,
            deadline: Instant::now() + Duration::from_secs(u64::from(MAX_SECONDS) + 10),
        })
    }

    pub fn completion(&self) -> Option<Result<()>> {
        if self.failed.load(Ordering::Acquire) {
            return Some(Err(anyhow::anyhow!(
                "Speech playback stopped. Check the playback device."
            )));
        }
        let finished = self.finished_at.lock().ok().and_then(|value| *value);
        if finished.is_some_and(|deadline| Instant::now() >= deadline) {
            return Some(Ok(()));
        }
        (Instant::now() >= self.deadline).then(|| {
            Err(anyhow::anyhow!(
                "The playback device did not finish the clip."
            ))
        })
    }
}

/// A callback delivering the final sample has only queued it for playback.
/// Keep the stream alive through the backend's predicted DAC timestamp.
/// Reject implausibly distant timestamps to bound that wait. CPAL receives a
/// finite stream initialization timeout, though not every backend honors it.
fn playback_stream<T>(
    device: &Device,
    config: StreamConfig,
    samples: Vec<i16>,
    failed: Arc<AtomicBool>,
    finished_at: Arc<Mutex<Option<Instant>>>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<i16>,
{
    let error = failed.clone();
    let mut offset = 0;
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], info| {
            let now = Instant::now();
            let timestamp = info.timestamp();
            let latency = timestamp
                .playback
                .checked_duration_since(timestamp.callback);
            if let Some(remaining) = render_samples(&samples, &mut offset, data, config.channels) {
                match (latency, finished_at.try_lock()) {
                    (Some(latency), Ok(mut deadline)) if latency <= Duration::from_secs(5) => {
                        *deadline = Some(
                            now + latency
                                + Duration::from_secs_f64(
                                    remaining as f64 / f64::from(config.sample_rate),
                                ),
                        );
                    }
                    _ => failed.store(true, Ordering::Release),
                }
            }
        },
        move |_| {
            error.store(true, Ordering::Release);
        },
        Some(Duration::from_secs(5)),
    )?)
}

fn render_samples<T>(
    samples: &[i16],
    offset: &mut usize,
    data: &mut [T],
    channels: u16,
) -> Option<usize>
where
    T: SizedSample + FromSample<i16>,
{
    let remaining = samples.len().saturating_sub(*offset);
    for frame in data.chunks_exact_mut(usize::from(channels)) {
        let value = samples.get(*offset).copied().unwrap_or(0);
        frame.fill(T::from_sample(value));
        *offset = offset.saturating_add(1);
    }
    (remaining > 0 && *offset >= samples.len()).then_some(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_averages_channels_without_growing_the_bounded_buffer() {
        let mut samples = Vec::with_capacity(3);
        let capacity = samples.capacity();
        assert!(!append_samples(
            &[i16::MIN, i16::MAX, 100, 300],
            2,
            3,
            &mut samples
        ));
        assert_eq!(samples, [0, 200]);
        assert!(append_samples(
            &[-300_i16, -100, 500, 500],
            2,
            3,
            &mut samples
        ));
        assert_eq!(samples, [0, 200, -200]);
        assert_eq!(samples.capacity(), capacity);
        assert!(append_samples(&[500_i16, 500], 2, 3, &mut samples));
        assert_eq!(samples, [0, 200, -200]);
    }

    #[test]
    fn capture_converts_float_and_unsigned_formats_to_signed_mono_pcm() {
        let mut samples = Vec::with_capacity(4);
        assert!(!append_samples(&[-1.0_f32, 0.0, 0.5], 1, 4, &mut samples));
        assert_eq!(samples, [i16::MIN, 0, 16384]);
        assert!(append_samples(&[32768_u16], 1, 4, &mut samples));
        assert_eq!(samples, [i16::MIN, 0, 16384, 0]);
    }

    #[test]
    fn playback_duplicates_mono_and_marks_only_the_callback_with_the_final_sample() {
        let mut offset = 0;
        let mut first = [0_i16; 4];
        assert_eq!(
            render_samples(&[100, -200, 300], &mut offset, &mut first, 2),
            None
        );
        assert_eq!(first, [100, 100, -200, -200]);
        let mut last = [1_i16; 4];
        assert_eq!(
            render_samples(&[100, -200, 300], &mut offset, &mut last, 2),
            Some(1)
        );
        assert_eq!(last, [300, 300, 0, 0]);
        assert_eq!(
            render_samples(&[100, -200, 300], &mut offset, &mut last, 2),
            None
        );
        assert_eq!(last, [0, 0, 0, 0]);
    }
}
