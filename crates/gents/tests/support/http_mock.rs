//! Shared request type for recorded mock-backend traffic.
//!
//! The mock servers are now axum-based (see `mock_endpoint` / `streaming_backend`),
//! so the previous hand-rolled `read_http_request` / `write_http_response`
//! helpers are gone. `HttpRequestData` survives as the shape
//! `MockModelEndpoint::recorded_requests` returns for assertion in tests.

use std::collections::HashMap;

#[derive(Clone)]
pub struct HttpRequestData {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}
