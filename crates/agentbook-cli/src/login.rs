//! OAuth login flow for Arda Gateway.
//!
//! Opens the user's browser to the Arda authorization page, listens for the
//! callback on a localhost port, exchanges the auth code for a `gw_sk_*` API
//! key, and stores it in the state directory.

use anyhow::{Context, Result};
use std::io::Write;
use std::net::TcpListener;
use std::time::Duration;

// ── Arda OAuth constants ────────────────────────────────────────────────────

/// OAuth client ID for the agentbook CLI app (registered on Arda Gateway).
/// This is a public identifier — safe to embed in source.
const ARDA_CLIENT_ID: &str = "gw_app_agentbook";

/// OAuth client secret. For first-party CLIs this is embedded in the binary
/// (same pattern as `gh auth login`, `gcloud auth login`, etc.).
/// TODO: Replace with actual secret after registering the OAuth app.
const ARDA_CLIENT_SECRET: &str = "gw_secret_PLACEHOLDER";

/// Arda Gateway base URL for API calls.
const ARDA_GATEWAY_URL: &str = "https://bot.ardabot.ai";

/// Arda web frontend URL for the OAuth authorization page.
const ARDA_AUTH_PAGE_URL: &str = "https://bot.ardabot.ai/connect";

/// How long to wait for the OAuth callback before timing out.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);

/// File name for the stored Arda API key.
pub const ARDA_KEY_FILE: &str = "arda_api_key";

/// File name for the stored Arda Gateway URL (so the TUI knows where to send
/// inference requests).
pub const ARDA_GATEWAY_URL_FILE: &str = "arda_gateway_url";

// ── Public API ──────────────────────────────────────────────────────────────

/// Run the full OAuth login flow: open browser, wait for callback, exchange
/// code for API key, store it.
pub async fn cmd_login() -> Result<()> {
    // Check if already logged in.
    let state_dir = agentbook_mesh::state_dir::default_state_dir()
        .context("unable to locate state directory")?;
    let key_path = state_dir.join(ARDA_KEY_FILE);
    if key_path.exists() {
        eprintln!(
            "\x1b[1;33mAlready logged in.\x1b[0m Run \x1b[1magentbook logout\x1b[0m first to re-authenticate."
        );
        return Ok(());
    }

    // 1. Bind a localhost callback server on a random high port.
    let listener = bind_callback_listener()?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    // 2. Generate a random state parameter to prevent CSRF.
    let state = generate_state();

    // 3. Build the authorization URL.
    let auth_url = format!(
        "{ARDA_AUTH_PAGE_URL}?client_id={ARDA_CLIENT_ID}&redirect_uri={redirect}&state={state}",
        redirect = urlencoded(&redirect_uri),
    );

    // 4. Open the browser (fall back to printing URL).
    eprintln!();
    eprintln!("  \x1b[1;36mOpening browser for Arda login...\x1b[0m");
    eprintln!();
    if !open_browser(&auth_url) {
        eprintln!("  Could not open browser. Visit this URL manually:");
        eprintln!();
        eprintln!("  \x1b[4m{auth_url}\x1b[0m");
        eprintln!();
    }
    eprintln!("  Waiting for authorization (timeout: {}s)...", CALLBACK_TIMEOUT.as_secs());

    // 5. Wait for the callback with the auth code.
    let code = wait_for_callback(listener, &state)?;

    // 6. Exchange the code for an API key.
    eprintln!("  Exchanging authorization code...");
    let api_key = exchange_code(&code, &redirect_uri).await?;

    // 7. Store the key and gateway URL.
    store_key(&state_dir, &api_key)?;
    store_gateway_url(&state_dir)?;

    eprintln!();
    eprintln!("  \x1b[1;32mLogged in successfully.\x1b[0m");
    eprintln!("  Sidekick will use Arda Gateway for inference.");
    eprintln!("  Manage your account at: \x1b[4m{ARDA_GATEWAY_URL}\x1b[0m");
    eprintln!();
    Ok(())
}

/// Delete the stored Arda API key.
pub fn cmd_logout() -> Result<()> {
    let state_dir = agentbook_mesh::state_dir::default_state_dir()
        .context("unable to locate state directory")?;
    let key_path = state_dir.join(ARDA_KEY_FILE);
    let url_path = state_dir.join(ARDA_GATEWAY_URL_FILE);

    if !key_path.exists() {
        eprintln!("Not logged in.");
        return Ok(());
    }

    std::fs::remove_file(&key_path).context("failed to delete API key file")?;
    let _ = std::fs::remove_file(&url_path);

    eprintln!("\x1b[1;32mLogged out.\x1b[0m Arda API key deleted.");
    eprintln!("Sidekick will fall back to direct Anthropic API key if available.");
    Ok(())
}

// ── Internals ───────────────────────────────────────────────────────────────

/// Try to bind a TCP listener on localhost with a random high port.
fn bind_callback_listener() -> Result<TcpListener> {
    // Try port 0 (OS picks a random available port).
    let listener =
        TcpListener::bind("127.0.0.1:0").context("failed to bind localhost callback server")?;
    listener
        .set_nonblocking(false)
        .context("failed to set listener to blocking mode")?;
    Ok(listener)
}

/// Generate a random state string for CSRF protection.
fn generate_state() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Minimal percent-encoding for URL query parameters.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(ch),
            _ => {
                for byte in ch.to_string().as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}

/// Try to open a URL in the user's default browser.
fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = url;
        false
    }
}

/// Wait for the OAuth callback on the localhost TCP listener. Parses the
/// `code` and `state` query parameters from the HTTP GET request.
fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String> {
    use std::io::Read;

    listener
        .set_nonblocking(false)
        .context("set blocking")?;

    // Set a timeout using SO_RCVTIMEO so we don't block forever.
    // TcpListener doesn't have set_timeout, so we use accept in a loop with
    // a deadline.
    let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;

    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Timed out waiting for OAuth callback. Login cancelled.");
        }

        // Use a short non-blocking poll.
        listener.set_nonblocking(true)?;
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;

                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                // Parse GET /callback?code=...&state=... HTTP/1.1
                if let Some(query) = parse_callback_query(&request) {
                    let code = query_param(&query, "code");
                    let state = query_param(&query, "state");

                    if let Some(code) = code {
                        // Validate state to prevent CSRF.
                        if state.as_deref() != Some(expected_state) {
                            send_http_response(
                                &mut stream,
                                "400 Bad Request",
                                "State mismatch. Please try again.",
                            );
                            anyhow::bail!("OAuth state mismatch (possible CSRF). Login cancelled.");
                        }

                        send_http_response(
                            &mut stream,
                            "200 OK",
                            "Login successful! You can close this tab and return to the terminal.",
                        );
                        return Ok(code);
                    }

                    // Check for error parameter.
                    if let Some(error) = query_param(&query, "error") {
                        let desc = query_param(&query, "error_description")
                            .unwrap_or_else(|| "Unknown error".to_string());
                        send_http_response(
                            &mut stream,
                            "400 Bad Request",
                            &format!("Authorization failed: {desc}"),
                        );
                        anyhow::bail!("OAuth error: {error} — {desc}");
                    }
                }

                // Not a callback request, ignore.
                send_http_response(&mut stream, "404 Not Found", "Not found");
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e).context("accept failed"),
        }
    }
}

/// Parse the query string from an HTTP GET request line.
fn parse_callback_query(request: &str) -> Option<String> {
    let first_line = request.lines().next()?;
    let path = first_line.split_whitespace().nth(1)?;
    if !path.starts_with("/callback") {
        return None;
    }
    path.find('?').map(|i| path[i + 1..].to_string())
}

/// Extract a query parameter value.
fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == name { Some(urldecode(v)) } else { None }
        })
}

/// Minimal percent-decoding.
fn urldecode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                &String::from_utf8_lossy(&bytes[i + 1..i + 3]),
                16,
            ) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Send a minimal HTTP response and close the connection.
fn send_http_response(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    let html = format!(
        "<!DOCTYPE html><html><body style='font-family:system-ui;text-align:center;padding:60px'>\
         <h2>{body}</h2></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Exchange the authorization code for an Arda Gateway API key.
async fn exchange_code(code: &str, redirect_uri: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{ARDA_GATEWAY_URL}/api/v1/oauth/token"))
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": ARDA_CLIENT_ID,
            "client_secret": ARDA_CLIENT_SECRET,
            "code": code,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await
        .context("failed to contact Arda Gateway")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed (HTTP {status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("invalid JSON from token endpoint")?;
    let api_key = body["data"]["api_key"]
        .as_str()
        .context("missing api_key in token response")?
        .to_string();

    if !api_key.starts_with("gw_sk_") {
        anyhow::bail!("unexpected API key format from Arda Gateway");
    }

    Ok(api_key)
}

/// Store the API key in the state directory with secure permissions.
fn store_key(state_dir: &std::path::Path, api_key: &str) -> Result<()> {
    std::fs::create_dir_all(state_dir).context("failed to create state directory")?;
    let path = state_dir.join(ARDA_KEY_FILE);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to write {}", path.display()))?;
        f.write_all(api_key.as_bytes())?;
        f.flush()?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(&path, api_key)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

/// Store the gateway URL so the TUI knows where to send inference requests.
fn store_gateway_url(state_dir: &std::path::Path) -> Result<()> {
    let path = state_dir.join(ARDA_GATEWAY_URL_FILE);
    std::fs::write(&path, ARDA_GATEWAY_URL).context("failed to write gateway URL file")?;
    Ok(())
}

// ── Key loading (shared with TUI) ──────────────────────────────────────────

/// Inference configuration resolved from stored keys.
#[derive(Debug, Clone)]
pub enum InferenceConfig {
    /// Arda Gateway: API key + gateway URL.
    Arda { key: String, gateway_url: String },
    /// Legacy direct Anthropic API key.
    Anthropic { key: String },
}

/// Load the inference configuration. Prefers Arda key, falls back to legacy
/// Anthropic key.
pub fn load_inference_config() -> Option<InferenceConfig> {
    let state_dir = agentbook_mesh::state_dir::default_state_dir().ok()?;

    // Prefer Arda Gateway key.
    let arda_key_path = state_dir.join(ARDA_KEY_FILE);
    if let Ok(raw) = std::fs::read_to_string(&arda_key_path) {
        let key = raw.trim().to_string();
        if !key.is_empty() {
            let gateway_url = std::fs::read_to_string(state_dir.join(ARDA_GATEWAY_URL_FILE))
                .unwrap_or_else(|_| ARDA_GATEWAY_URL.to_string())
                .trim()
                .to_string();
            return Some(InferenceConfig::Arda { key, gateway_url });
        }
    }

    // Fall back to legacy Anthropic key (from env or file).
    if let Ok(key) = std::env::var("AGENTBOOK_ANTHROPIC_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(InferenceConfig::Anthropic { key });
        }
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(InferenceConfig::Anthropic { key });
        }
    }

    let anthropic_path = state_dir.join("sidekick_anthropic_api_key");
    if let Ok(raw) = std::fs::read_to_string(anthropic_path) {
        let key = raw.trim().to_string();
        if !key.is_empty() {
            return Some(InferenceConfig::Anthropic { key });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_urlencoded() {
        assert_eq!(urlencoded("hello world"), "hello%20world");
        assert_eq!(
            urlencoded("http://localhost:8080/callback"),
            "http%3A%2F%2Flocalhost%3A8080%2Fcallback"
        );
    }

    #[test]
    fn test_urldecode() {
        assert_eq!(urldecode("hello%20world"), "hello world");
        assert_eq!(urldecode("a+b"), "a b");
        assert_eq!(urldecode("gw_code_abc123"), "gw_code_abc123");
    }

    #[test]
    fn test_parse_callback_query() {
        let req = "GET /callback?code=gw_code_abc&state=xyz123 HTTP/1.1\r\nHost: localhost\r\n";
        let query = parse_callback_query(req).unwrap();
        assert_eq!(query_param(&query, "code").unwrap(), "gw_code_abc");
        assert_eq!(query_param(&query, "state").unwrap(), "xyz123");
    }

    #[test]
    fn test_parse_callback_query_not_callback() {
        let req = "GET /favicon.ico HTTP/1.1\r\n";
        assert!(parse_callback_query(req).is_none());
    }

    #[test]
    fn test_store_and_load_key() {
        let dir = TempDir::new().unwrap();
        store_key(dir.path(), "gw_sk_test123").unwrap();

        let content = std::fs::read_to_string(dir.path().join(ARDA_KEY_FILE)).unwrap();
        assert_eq!(content, "gw_sk_test123");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(dir.path().join(ARDA_KEY_FILE)).unwrap();
            assert_eq!(meta.mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn test_store_gateway_url() {
        let dir = TempDir::new().unwrap();
        store_gateway_url(dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join(ARDA_GATEWAY_URL_FILE)).unwrap();
        assert_eq!(content, ARDA_GATEWAY_URL);
    }

    #[test]
    fn test_generate_state_is_unique() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(s1, s2);
        assert_eq!(s1.len(), 32); // 16 bytes as hex
    }

    #[test]
    fn test_logout_when_not_logged_in() {
        // Should not error, just print message.
        // We can't easily test this without mocking state_dir, so just test the
        // store/delete round trip.
        let dir = TempDir::new().unwrap();
        let key_path = dir.path().join(ARDA_KEY_FILE);

        // Not logged in — file doesn't exist.
        assert!(!key_path.exists());

        // Store a key.
        store_key(dir.path(), "gw_sk_test").unwrap();
        assert!(key_path.exists());

        // Delete it.
        std::fs::remove_file(&key_path).unwrap();
        assert!(!key_path.exists());
    }
}
