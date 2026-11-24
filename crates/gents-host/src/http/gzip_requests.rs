//! Gzip on the host's HTTP routes, so a client on a slow or lossy link moves
//! fewer bytes. The host inflates a gzip request body
//! (`Content-Encoding: gzip`) and says on every answer that it does
//! (`Accept-Encoding: gzip` on a response, RFC 7694); a client gzips a large
//! body only toward a host whose latest answer said so (Lean
//! `RequestEncoding`). The host also gzips a large JSON answer, only for a
//! request that carried `Accept-Encoding: gzip`.
//! Event streams are left alone: compressing them would hold events back in
//! the encoder.

use axum::{
    body::{Body, HttpBody as _},
    extract::Request,
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::io::{Read as _, Write as _};

/// Largest compressed or inflated body accepted, and largest answer gzipped.
const LIMIT: usize = 16 * 1024 * 1024;

/// JSON answers larger than this many bytes are sent gzipped.
const COMPRESS_OVER: usize = 4 * 1024;

pub fn wrap(router: axum::Router) -> axum::Router {
    router.layer(axum::middleware::from_fn(gzip))
}

fn names_gzip(value: Option<&HeaderValue>) -> bool {
    value
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|coding| {
                coding
                    .split(';')
                    .next()
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case("gzip"))
            })
        })
}

async fn gzip(request: Request, next: Next) -> Response {
    let accepts_gzip = names_gzip(request.headers().get(header::ACCEPT_ENCODING));
    let mut response = match inflate(request).await {
        Ok(request) => next.run(request).await,
        Err(refusal) => refusal.into_response(),
    };
    response
        .headers_mut()
        .entry(header::ACCEPT_ENCODING)
        .or_insert(HeaderValue::from_static("gzip"));
    if accepts_gzip {
        compress(response).await
    } else {
        response
    }
}

/// The request with its gzip body inflated, or the status refusing it: 413
/// over the limit, 400 for a body that is not gzip.
async fn inflate(request: Request) -> Result<Request, StatusCode> {
    if !names_gzip(request.headers().get(header::CONTENT_ENCODING)) {
        return Ok(request);
    }
    let (mut parts, body) = request.into_parts();
    let compressed = axum::body::to_bytes(body, LIMIT)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let inflated = tokio::task::spawn_blocking(move || inflate_bytes(&compressed))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
    parts.headers.remove(header::CONTENT_ENCODING);
    parts
        .headers
        .insert(header::CONTENT_LENGTH, inflated.len().into());
    Ok(Request::from_parts(parts, Body::from(inflated)))
}

/// A gzip body of up to [`LIMIT`] is tens of milliseconds of codec work, so
/// it runs on the blocking pool, never on a runtime worker.
fn inflate_bytes(compressed: &[u8]) -> Result<Vec<u8>, StatusCode> {
    let mut inflated = Vec::new();
    flate2::read::GzDecoder::new(compressed)
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut inflated)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if inflated.len() > LIMIT {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(inflated)
}

/// Runs on the blocking pool, like [`inflate_bytes`].
fn deflate(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

async fn compress(response: Response) -> Response {
    let json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    let size = response.body().size_hint().exact();
    let worth = size.is_some_and(|size| size > COMPRESS_OVER as u64 && size <= LIMIT as u64);
    if !json || !worth || response.headers().contains_key(header::CONTENT_ENCODING) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, LIMIT).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let plain = bytes.clone();
    let Some(compressed) = tokio::task::spawn_blocking(move || deflate(&plain))
        .await
        .ok()
        .flatten()
    else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    parts
        .headers
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .append(header::VARY, HeaderValue::from_static("accept-encoding"));
    Response::from_parts(parts, Body::from(compressed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What crossed the wire: each request's and each answer's
    /// `Content-Encoding`, and the body the handler read.
    #[derive(Default)]
    struct Seen {
        encodings: Mutex<Vec<Option<String>>>,
        answers: Mutex<Vec<Option<String>>>,
        bodies: Mutex<Vec<serde_json::Value>>,
    }

    fn router(seen: Arc<Seen>, answer: serde_json::Value) -> axum::Router {
        let handler_seen = seen.clone();
        axum::Router::new().route(
            "/api/v0/graphql",
            axum::routing::post(move |body: axum::body::Bytes| {
                let seen = handler_seen.clone();
                let answer = answer.clone();
                async move {
                    seen.bodies
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    axum::Json(answer)
                }
            }),
        )
    }

    /// Records the encoding a request arrived with, outside every other layer.
    fn recording(router: axum::Router, seen: Arc<Seen>) -> axum::Router {
        router.layer(axum::middleware::from_fn(
            move |request: Request, next: Next| {
                let seen = seen.clone();
                async move {
                    let encoding = request
                        .headers()
                        .get(header::CONTENT_ENCODING)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    seen.encodings.lock().unwrap().push(encoding);
                    let response = next.run(request).await;
                    let encoding = response
                        .headers()
                        .get(header::CONTENT_ENCODING)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    seen.answers.lock().unwrap().push(encoding);
                    response
                }
            },
        ))
    }

    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}/api/v0/graphql")
    }

    /// A read whose request body is well over the gzip threshold.
    fn large_read() -> String {
        format!(
            r#"{{ Agent(filter: {{ name: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            "a".repeat(62_000)
        )
    }

    /// One read by a client whose handle for `graphql` holds `encoding`.
    async fn read(
        encoding: &gents_protocol::graphql::RequestEncoding,
        graphql: &str,
    ) -> serde_json::Value {
        gents_protocol::graphql::execute_graphql_encoded(
            encoding,
            graphql,
            None,
            &large_read(),
            None,
            gents_protocol::graphql::GraphqlRequestOptions::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_host_that_does_not_advertise_gzip_gets_plain_bodies() {
        let seen = Arc::new(Seen::default());
        let answer = serde_json::json!({ "data": { "Agent": [] } });
        let graphql = serve(recording(
            router(seen.clone(), answer.clone()),
            seen.clone(),
        ))
        .await;
        let encoding = gents_protocol::graphql::RequestEncoding::default();
        for _ in 0..3 {
            assert_eq!(read(&encoding, &graphql).await, answer);
        }
        assert_eq!(*seen.encodings.lock().unwrap(), vec![None, None, None]);
        assert_eq!(*seen.answers.lock().unwrap(), vec![None, None, None]);
    }

    #[tokio::test]
    async fn a_host_that_advertises_gzip_gets_gzipped_bodies_and_inflates_them() {
        let seen = Arc::new(Seen::default());
        /* an answer large enough that the host gzips it back */
        let answer = serde_json::json!({ "data": { "Agent": [{ "_docID": "b".repeat(8_000) }] } });
        let graphql = serve(recording(
            wrap(router(seen.clone(), answer.clone())),
            seen.clone(),
        ))
        .await;
        /* the first request learns that the host accepts gzip */
        let encoding = gents_protocol::graphql::RequestEncoding::default();
        assert_eq!(read(&encoding, &graphql).await, answer);
        assert_eq!(read(&encoding, &graphql).await, answer);
        assert_eq!(
            *seen.encodings.lock().unwrap(),
            vec![None, Some("gzip".to_owned())]
        );
        let gzipped = Some("gzip".to_owned());
        assert_eq!(
            *seen.answers.lock().unwrap(),
            vec![gzipped.clone(), gzipped],
            "the host gzipped the large answers, and the client read them"
        );
        let bodies = seen.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0], bodies[1], "the host inflated the gzipped body");
        assert_eq!(bodies[1]["query"], large_read());
    }

    #[tokio::test]
    async fn a_client_that_does_not_ask_for_gzip_gets_plain_answers() {
        let seen = Arc::new(Seen::default());
        let answer = serde_json::json!({ "data": { "Agent": [{ "_docID": "b".repeat(8_000) }] } });
        let graphql = serve(recording(
            wrap(router(seen.clone(), answer.clone())),
            seen.clone(),
        ))
        .await;
        let response = reqwest::Client::new()
            .post(&graphql)
            .json(&serde_json::json!({ "query": "{ Agent { _docID } }" }))
            .send()
            .await
            .unwrap();
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(response.json::<serde_json::Value>().await.unwrap(), answer);
        assert_eq!(*seen.answers.lock().unwrap(), vec![None]);
    }

    /// While a large body is inflated or a large answer gzipped, the runtime
    /// thread serves its other tasks: on a single-threaded runtime, a task
    /// polled after the codec starts still runs before the codec finishes.
    #[tokio::test(flavor = "current_thread")]
    async fn the_codec_leaves_the_runtime_thread_free() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let large = br#"{"data":"#.repeat(1 << 17);
        let free = AtomicBool::new(false);
        let request = Request::builder()
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(deflate(&large).unwrap()))
            .unwrap();
        let ((inflated, free_while_inflating), ()) = tokio::join!(
            biased;
            async { (inflate(request).await, free.load(Ordering::SeqCst)) },
            async { free.store(true, Ordering::SeqCst) },
        );
        assert!(free_while_inflating, "inflating held the runtime thread");
        let inflated = axum::body::to_bytes(inflated.unwrap().into_body(), LIMIT)
            .await
            .unwrap();
        assert_eq!(inflated.as_ref(), large.as_slice());

        free.store(false, Ordering::SeqCst);
        let response = Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(large.clone()))
            .unwrap();
        let ((compressed, free_while_compressing), ()) = tokio::join!(
            biased;
            async { (compress(response).await, free.load(Ordering::SeqCst)) },
            async { free.store(true, Ordering::SeqCst) },
        );
        assert!(free_while_compressing, "gzipping held the runtime thread");
        assert_eq!(compressed.headers()[header::CONTENT_ENCODING], "gzip");
        let compressed = axum::body::to_bytes(compressed.into_body(), LIMIT)
            .await
            .unwrap();
        assert_eq!(inflate_bytes(&compressed).unwrap(), large);
    }

    #[tokio::test]
    async fn a_body_that_is_not_gzip_is_refused() {
        let seen = Arc::new(Seen::default());
        let graphql = serve(wrap(router(seen, serde_json::json!({})))).await;
        let response = reqwest::Client::new()
            .post(&graphql)
            .header(header::CONTENT_ENCODING, "gzip")
            .body("not gzip")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(header::ACCEPT_ENCODING).unwrap(),
            "gzip"
        );
    }
}
