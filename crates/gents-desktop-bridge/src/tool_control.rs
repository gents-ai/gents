use std::future::Future;
use std::time::Duration;

use gents_desktop_core::client::{ClientCore, StoreUpdateNotice};
use gents_protocol::process_control::ToolControlReceiptOutcome;
use tokio::sync::watch;

const OBSERVATION_DEADLINE: Duration = Duration::from_secs(30);

pub(crate) fn unconfirmed_submission(intent_id: &str, detail: &str) -> String {
    format!("Control intent {intent_id} was not confirmed after submission; the action may still take effect. {detail}")
}

pub(crate) async fn send(
    request: reqwest::RequestBuilder,
    intent_id: &str,
) -> Result<reqwest::Response, String> {
    request
        .send()
        .await
        .map_err(|error| unconfirmed_submission(intent_id, &error.to_string()))
}

async fn bounded<T, F>(intent_doc_id: &str, deadline: Duration, future: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    tokio::time::timeout(deadline, future).await.unwrap_or_else(|_| {
        Err(format!("Control {intent_doc_id} was not confirmed before the observation deadline; it may still complete."))
    })
}

pub(crate) async fn await_receipt(
    core: &ClientCore,
    node_did: &str,
    intent_doc_id: &str,
    tool_call_doc_id: &str,
    action: &str,
    mut changes: watch::Receiver<StoreUpdateNotice>,
) -> Result<ToolControlReceiptOutcome, String> {
    bounded(intent_doc_id, OBSERVATION_DEADLINE, async {
        loop {
            match core.tool_control_receipt(node_did, intent_doc_id, tool_call_doc_id, action).await {
                Ok(Some(receipt)) => return Ok(receipt.outcome),
                Ok(None) => {}
                Err(error) => return Err(format!("Control {intent_doc_id} could not be observed: {error:#}; its outcome is unconfirmed.")),
            }
            changes.changed().await.map_err(|_| format!("Control {intent_doc_id} observation stopped; its outcome is unconfirmed."))?;
        }
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deadline_reports_the_control_reference_without_claiming_rollback() {
        let error = bounded::<(), _>("intent-123", Duration::ZERO, std::future::pending())
            .await
            .unwrap_err();
        assert!(error.contains("intent-123"));
        assert!(error.contains("observation deadline"));
        assert!(error.contains("may still complete"));
    }

    #[tokio::test]
    async fn dropped_response_reports_the_signed_intent_as_unconfirmed() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let count = socket.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0, "request closed before its signed intent arrived");
                bytes.extend_from_slice(&chunk[..count]);
                if bytes
                    .windows(b"intent-123".len())
                    .any(|window| window == b"intent-123")
                {
                    break;
                }
            }
        });
        let request = reqwest::Client::new()
            .post(format!("http://{address}/control"))
            .body("intent-123");
        let error = tokio::time::timeout(Duration::from_secs(5), send(request, "intent-123"))
            .await
            .expect("dropped-response observation timed out")
            .unwrap_err();
        server.await.unwrap();
        assert!(error.contains("intent-123"));
        assert!(error.contains("not confirmed"));
        assert!(error.contains("may still take effect"));
    }
}
