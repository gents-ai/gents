use std::sync::atomic::AtomicBool;

#[cfg(not(target_os = "macos"))]
pub fn authorize(_stopped: &AtomicBool) -> anyhow::Result<()> {
    Ok(())
}

/// CoreAudio can return silence before microphone authorization. Request it
/// only after explicit capture, before opening a stream. AVFoundation reports
/// its decision on an arbitrary dispatch queue, so no UI run loop is required.
/// A pending system prompt cannot keep capture open indefinitely, and the
/// bounded receive intervals also let cancellation interrupt the wait.
#[cfg(target_os = "macos")]
pub fn authorize(stopped: &AtomicBool) -> anyhow::Result<()> {
    use anyhow::{bail, ensure, Context};
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    ensure!(
        !stopped.load(Ordering::Acquire),
        "Microphone capture cancelled."
    );
    // SAFETY: AVMediaTypeAudio is Apple's audio media constant, one of the
    // two values accepted by these authorization methods.
    let media = unsafe { AVMediaTypeAudio }
        .context("Microphone authorization is unavailable on this system.")?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    ensure!(status == AVAuthorizationStatus::NotDetermined,
        "Microphone access is denied or restricted. Allow Gents Voice or Visual Studio Code in System Settings > Privacy & Security > Microphone.");
    let (sender, result) = std::sync::mpsc::sync_channel(1);
    let completion = block2::RcBlock::new(move |granted: Bool| {
        let _ = sender.try_send(granted.as_bool());
    });
    // SAFETY: the audio constant is valid and AVFoundation retains this
    // completion block until it reports the permission decision.
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &completion);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        ensure!(
            !stopped.load(Ordering::Acquire),
            "Microphone capture cancelled."
        );
        ensure!(Instant::now() < deadline, "Microphone permission is still pending. Respond to the system prompt, then start dictation again.");
        match result.recv_timeout(Duration::from_millis(20)) {
            Ok(true) => return Ok(()),
            Ok(false) => bail!("Microphone access was denied. Allow it in System Settings > Privacy & Security > Microphone."),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {},
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => bail!("Microphone authorization ended before returning a decision."),
        }
    }
}
