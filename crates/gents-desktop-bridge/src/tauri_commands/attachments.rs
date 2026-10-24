use crate::{
    error::BridgeError,
    state::{current_core, DesktopAppState},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use gents_protocol::request_input::RequestAttachment;
use std::io::Read;
use tauri::State;
use tauri_plugin_fs::FsExt;

#[tauri::command]
pub async fn desktop_attachment_read<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<RequestAttachment, BridgeError> {
    let scope = app
        .try_fs_scope()
        .ok_or_else(|| BridgeError::untyped("Native file selection is unavailable"))?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<RequestAttachment> {
        let path = std::path::Path::new(&path);
        anyhow::ensure!(path.is_absolute(), "Choose an absolute file path");
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment")
            .to_owned();
        let path = path.canonicalize()?;
        anyhow::ensure!(
            scope.is_allowed(&path),
            "Select this file in the picker or drop it into the app before attaching it"
        );
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        anyhow::ensure!(file.metadata()?.is_file(), "Choose a file, not a directory");
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(!bytes.is_empty(), "The file is empty");
        anyhow::ensure!(bytes.len() <= 4 * 1024 * 1024, "Attachment exceeds 4 MiB");
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            "image/webp"
        } else if bytes.starts_with(b"%PDF-") {
            "application/pdf"
        } else if std::str::from_utf8(&bytes).is_ok() && !bytes.contains(&0) {
            "text/plain"
        } else {
            anyhow::bail!("Unsupported file. Choose PNG, JPEG, GIF, WebP, PDF or UTF-8 text")
        };
        let attachment = RequestAttachment {
            name,
            media_type: mime.into(),
            data: STANDARD.encode(bytes),
        };
        attachment.validate()?;
        Ok(attachment)
    })
    .await
    .map_err(|e| BridgeError::untyped(e.to_string()))?
    .map_err(|e| BridgeError::untyped(e.to_string()))
}

#[tauri::command]
pub async fn desktop_request_attachments(
    node_did: String,
    session_id: String,
    request_id: String,
    message_key: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<RequestAttachment>, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("Desktop client is not running"))?;
    if let Some(message_key) = message_key {
        return crate::commands::message_attachments(&core, &session_id, &node_did, &message_key)
            .await;
    }
    let row = core
        .request_in_scope(&request_id, Some(&node_did))
        .map_err(|e| BridgeError::untyped(e.to_string()))?;
    if row.session_id.as_deref() != Some(&session_id) {
        return Err(BridgeError::untyped(
            "Attachment request belongs to another session",
        ));
    }
    Ok(row.input.unwrap_or_default().attachments)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn file_selection_freezes_bytes_and_rejects_directories_and_oversize_files() {
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_fs::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "original bytes").unwrap();
        assert!(
            desktop_attachment_read(app.handle().clone(), path.to_str().unwrap().into())
                .await
                .is_err()
        );
        app.fs_scope().allow_file(&path).unwrap();
        let captured = desktop_attachment_read(app.handle().clone(), path.to_str().unwrap().into())
            .await
            .unwrap();
        std::fs::write(&path, "changed later").unwrap();
        assert_eq!(captured.name, "note.txt");
        assert_eq!(captured.media_type, "text/plain");
        assert_eq!(STANDARD.decode(&captured.data).unwrap(), b"original bytes");
        assert!(
            desktop_attachment_read(app.handle().clone(), dir.path().to_str().unwrap().into())
                .await
                .is_err()
        );
        std::fs::File::create(&path)
            .unwrap()
            .set_len(4 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            desktop_attachment_read(app.handle().clone(), path.to_str().unwrap().into())
                .await
                .is_err()
        );
    }
}
