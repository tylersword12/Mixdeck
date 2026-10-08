// Small shared helpers: API errors, ids, time, lenient JSON readers.
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::RngCore;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

/// Error returned from API handlers. Rendered as `{ "error": "..." }`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, msg: impl Into<String>) -> Self {
        ApiError { status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), message: msg.into() }
    }
    pub fn bad(msg: impl Into<String>) -> Self { Self::new(400, msg) }
    pub fn not_found(msg: impl Into<String>) -> Self { Self::new(404, msg) }
    pub fn internal(msg: impl Into<String>) -> Self { Self::new(500, msg) }
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = StatusCode::from_u16(status).unwrap_or(self.status);
        self
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError::internal(e.to_string()) }
}
impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self { ApiError::new(502, format!("Network error: {}", short_reqwest(&e))) }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self { ApiError::internal(e.to_string()) }
}
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self { ApiError::new(502, format!("Unexpected response: {e}")) }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Matches the old server: "not logged in" style 401s are reported as 409 so the
        // browser doesn't treat them as HTTP auth challenges.
        let status = if self.status == StatusCode::UNAUTHORIZED { StatusCode::CONFLICT } else { self.status };
        if status.is_server_error() {
            crate::logger::log(format!("[error] {}", self.message));
        }
        (status, [("Cache-Control", "no-store")], Json(json!({ "error": self.message }))).into_response()
    }
}

pub type ApiResult<T = Value> = Result<T, ApiError>;

pub fn short_reqwest(e: &reqwest::Error) -> String {
    if e.is_timeout() { return "timed out".into(); }
    if e.is_connect() { return "could not connect (check your internet connection)".into(); }
    let s = e.to_string();
    s.chars().take(200).collect()
}

pub fn now_ms() -> i64 { chrono::Utc::now().timestamp_millis() }

pub fn uid() -> String {
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    hex(&b)
}

pub fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

pub fn b64url(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// JS-style truthiness for values coming from the web pages.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JS-style Number(x) with a fallback for NaN.
pub fn num(v: &Value, fallback: f64) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(fallback),
        Value::String(s) => s.trim().parse().unwrap_or(fallback),
        Value::Bool(b) => if *b { 1.0 } else { 0.0 },
        _ => fallback,
    }
}

/// String value of a JSON field ("" when missing), accepting numbers too.
pub fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

pub fn de_string_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(s(&v))
}

pub fn de_f64_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(num(&v, 0.0))
}

pub fn de_opt_string_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Null => None,
        other => Some(s(&other)),
    })
}

/// Truncate to at most `n` characters (not bytes).
pub fn clip(s: &str, n: usize) -> String { s.chars().take(n).collect() }

pub fn url_encode(s: &str) -> String { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() }
