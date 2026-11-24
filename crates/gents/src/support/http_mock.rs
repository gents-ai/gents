use rapidhash::RapidHashMap;

#[derive(Clone)]
pub struct HttpRequestData {
    pub method: String,
    pub path: String,
    pub headers: RapidHashMap<String, String>,
    pub body: String,
}
