//! HTTP requests for `*.url` keys and REQ_URL notifications, mirroring
//! upstream helpers.request() including its quirks:
//!
//! - retry on [500, 502, 503, 504] unless enable_5xx, with urllib3-style
//!   backoff (factor * 2^(n-1), no sleep before the first retry);
//! - a response with status >= 400 is *falsy* in Python (`Response.__bool__`
//!   is `ok`), so callers receive EMPTY content for it — see NOTES.md;
//! - on exhausted retries or transport errors, log and return a dummy empty
//!   response rather than an error.

use std::sync::OnceLock;
use std::time::Duration;

use crate::logger;

pub struct HttpCfg {
    pub retry_total: u32,
    pub backoff_factor: f64,
    pub timeout: f64,
    pub skip_tls_verify: bool,
    pub username_file_flag: Option<String>,
    pub password_file_flag: Option<String>,
}

static CFG: OnceLock<HttpCfg> = OnceLock::new();

fn envf(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn envu(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read once at startup, like upstream's module-level constants.
pub fn init(username_file_flag: Option<String>, password_file_flag: Option<String>) {
    let _ = CFG.set(HttpCfg {
        retry_total: envu("REQ_RETRY_TOTAL", 5),
        backoff_factor: envf("REQ_RETRY_BACKOFF_FACTOR", 1.1),
        timeout: envf("REQ_TIMEOUT", 10.0),
        skip_tls_verify: std::env::var("REQ_SKIP_TLS_VERIFY").as_deref() == Ok("true"),
        username_file_flag,
        password_file_flag,
    });
}

pub fn cfg() -> &'static HttpCfg {
    CFG.get_or_init(|| HttpCfg {
        retry_total: 5,
        backoff_factor: 1.1,
        timeout: 10.0,
        skip_tls_verify: false,
        username_file_flag: None,
        password_file_flag: None,
    })
}

fn read_file_content(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Some(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            logger::warning(&format!("File not found at: {}", path));
            None
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            logger::error(&format!("No read permission for file: {}", path));
            None
        }
        Err(e) => {
            logger::error(&format!("An unexpected error occurred: {}", e));
            None
        }
    }
}

fn fetch_basic_auth_credentials() -> (Option<String>, Option<String>) {
    let c = cfg();
    let mut username = std::env::var("REQ_USERNAME").ok();
    let mut password = std::env::var("REQ_PASSWORD").ok();
    // CLI flags take precedence over the *_FILE env vars; file contents
    // override the plain env vars only when the file is readable.
    let user_file = c
        .username_file_flag
        .clone()
        .or_else(|| std::env::var("REQ_USERNAME_FILE").ok());
    let pass_file = c
        .password_file_flag
        .clone()
        .or_else(|| std::env::var("REQ_PASSWORD_FILE").ok());
    if let Some(f) = user_file
        && let Some(v) = read_file_content(&f)
    {
        username = Some(v);
    }
    if let Some(f) = pass_file
        && let Some(v) = read_file_content(&f)
    {
        password = Some(v);
    }
    (username, password)
}

/// Encode a credential in the configured encoding (default latin1, like
/// upstream). Unmappable characters become '?' rather than erroring.
fn encode_cred(s: &str, encoding: &str) -> Vec<u8> {
    match encoding.to_lowercase().as_str() {
        "latin1" | "latin-1" | "iso-8859-1" | "ascii" => s
            .chars()
            .map(|ch| {
                let cp = ch as u32;
                if cp <= 0xFF { cp as u8 } else { b'?' }
            })
            .collect(),
        _ => s.as_bytes().to_vec(),
    }
}

fn basic_auth_header() -> Option<String> {
    let (user, pass) = fetch_basic_auth_credentials();
    let (user, pass) = (user?, pass?);
    if user.is_empty() || pass.is_empty() {
        // Python: `if username and password` — empty strings are falsy.
        return None;
    }
    let encoding = std::env::var("REQ_BASIC_AUTH_ENCODING").unwrap_or_else(|_| "latin1".into());
    let mut raw = encode_cred(&user, &encoding);
    raw.push(b':');
    raw.extend(encode_cred(&pass, &encoding));
    use base64::Engine;
    Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    ))
}

/// Mirror of helpers.request(). Returns Some((status, body)) when a response
/// was obtained, None for the dummy/no-op cases (no url, invalid method,
/// exhausted retries, transport error). NOTE: callers wanting content must
/// apply the falsy-response rule via [`body_if_ok`].
pub async fn request(
    url: Option<&str>,
    method: Option<&str>,
    enable_5xx: bool,
    payload: Option<&serde_json::Value>,
) -> Option<(u16, Vec<u8>)> {
    let c = cfg();
    let enforce_status: &[u16] = if enable_5xx {
        &[]
    } else {
        &[500, 502, 503, 504]
    };

    let auth = basic_auth_header();

    let Some(url) = url else {
        logger::warning("No url provided. Doing nothing.");
        return None;
    };

    let method_up = method.unwrap_or("GET");
    let is_get = method.is_none() || method == Some("GET");
    let is_post = method == Some("POST");
    if !is_get && !is_post {
        logger::warning(&format!(
            "Invalid REQ_METHOD: '{}', please use 'GET' or 'POST'. Doing nothing.",
            method_up
        ));
        return None;
    }

    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(c.skip_tls_verify)
        .timeout(Duration::from_secs_f64(c.timeout))
        .build()
    {
        Ok(cl) => cl,
        Err(e) => {
            logger::error(&format!(
                "Unexpected error during request to {}: {}",
                url, e
            ));
            logger::debug(&format!("Returning dummy response for URL {}", url));
            return None;
        }
    };

    let mut last_err: Option<String> = None;
    let attempts = c.retry_total + 1;
    for attempt in 0..attempts {
        if attempt > 1 {
            // urllib3: no sleep before the first retry, then factor * 2^(n-1).
            let backoff = (c.backoff_factor * 2f64.powi(attempt as i32 - 1)).min(120.0);
            tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
        }
        let mut req = if is_get {
            client.get(url)
        } else {
            client.post(url)
        };
        if let Some(a) = &auth {
            req = req.header(reqwest::header::AUTHORIZATION, a.clone());
        }
        if is_post {
            req = req.json(payload.unwrap_or(&serde_json::Value::Null));
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if enforce_status.contains(&status.as_u16()) {
                    last_err = Some(format!(
                        "too many {} error responses (Caused by ResponseError('too many {} error responses'))",
                        status.as_u16(),
                        status.as_u16()
                    ));
                    continue;
                }
                let body = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                let text = String::from_utf8_lossy(&body);
                let reason = status.canonical_reason().unwrap_or("");
                if is_get {
                    logger::info(&format!(
                        "Request sent to {}. Response: {} {} {}",
                        url,
                        status.as_u16(),
                        reason,
                        text
                    ));
                } else {
                    let payload_repr = payload
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "None".into());
                    logger::info(&format!(
                        "{} sent to {}. Response: {} {} {}",
                        payload_repr,
                        url,
                        status.as_u16(),
                        reason,
                        text
                    ));
                }
                return Some((status.as_u16(), body));
            }
            Err(e) => {
                last_err = Some(e.to_string());
                continue;
            }
        }
    }

    logger::error(&format!(
        "Max retries exceeded for URL {}: {}",
        url,
        last_err.unwrap_or_default()
    ));
    logger::debug(&format!("Returning dummy response for URL {}", url));
    None
}

/// The Python falsy-response rule: content only for status < 400.
pub fn body_if_ok(resp: Option<(u16, Vec<u8>)>) -> Vec<u8> {
    match resp {
        Some((status, body)) if status < 400 => body,
        _ => Vec::new(),
    }
}
