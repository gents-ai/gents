use std::sync::atomic::AtomicBool;

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("Microphone capture cancelled.")]
    Cancelled,
    #[error("Microphone authorization is unavailable on this system.")]
    Unavailable,
    #[error("Microphone access is denied or restricted. Allow Gents Voice or Visual Studio Code in System Settings > Privacy & Security > Microphone.")]
    Restricted,
    #[error("Microphone permission is still pending. Respond to the system prompt, then start dictation again.")]
    Pending,
    #[error("Microphone access was denied. Allow it in System Settings > Privacy & Security > Microphone.")]
    Denied,
    #[error("Microphone authorization ended before returning a decision.")]
    NoDecision,
}

#[cfg(not(target_os = "macos"))]
pub fn authorize(_stopped: &AtomicBool) -> Result<(), PermissionError> {
    Ok(())
}

/// CoreAudio can return silence before microphone authorization. Request it
/// only after explicit capture, before opening a stream. AVFoundation reports
/// its decision on an arbitrary dispatch queue, so no UI run loop is required.
#[cfg(target_os = "macos")]
pub fn authorize(stopped: &AtomicBool) -> Result<(), PermissionError> {
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    if stopped.load(Ordering::Acquire) {
        return Err(PermissionError::Cancelled);
    }
    // SAFETY: AVMediaTypeAudio is Apple's audio media constant, one of the
    // two values accepted by these authorization methods.
    let media = unsafe { AVMediaTypeAudio }.ok_or(PermissionError::Unavailable)?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    if status != AVAuthorizationStatus::NotDetermined {
        return Err(PermissionError::Restricted);
    }
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
        if stopped.load(Ordering::Acquire) {
            return Err(PermissionError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(PermissionError::Pending);
        }
        match result.recv_timeout(Duration::from_millis(20)) {
            Ok(true) => return Ok(()),
            Ok(false) => return Err(PermissionError::Denied),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(PermissionError::NoDecision)
            }
        }
    }
}
