use std::collections::HashMap;

#[derive(Clone)]
pub struct HttpRequestData {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}
