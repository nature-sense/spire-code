//! Talking to a board's HTTP surface — the upload endpoint next to its MCP one.
//!
//! MCP tool calls to a board go through `spire-core`'s MCP client; pushing a
//! cross-built test binary to it is plain HTTP, so that lives here.

use std::time::Duration;

/// Path of the device server's upload endpoint.
pub const UPLOAD_PATH: &str = "/upload";

/// Timeout for a single upload. Test binaries are small; a stalled board must
/// not hang the caller forever.
pub const UPLOAD_TIMEOUT_SECS: u64 = 120;

/// Outcome of a successful upload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Uploaded {
    pub url: String,
    pub name: String,
    pub bytes: usize,
}

/// The board's upload endpoint, derived from its MCP URL.
///
/// `http://rpi5.local:8737/mcp` → `http://rpi5.local:8737/upload`. Any path on
/// the MCP URL is replaced (never appended to), so `/mcp`, `/` and a bare
/// `host:port` all resolve to the same place.
pub fn upload_endpoint(mcp_url: &str) -> Result<String, String> {
    let url = mcp_url.trim();
    if url.is_empty() {
        return Err("device.mcp.url is empty".to_string());
    }

    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("device.mcp.url has no scheme: {url}"))?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(format!("device.mcp.url has no host: {url}"));
    }

    Ok(format!("{scheme}://{authority}{UPLOAD_PATH}"))
}

/// The full upload URL for `name`.
pub fn upload_url(mcp_url: &str, name: &str) -> Result<String, String> {
    let endpoint = upload_endpoint(mcp_url)?;
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(format!("invalid upload name: {name:?}"));
    }
    Ok(format!("{endpoint}/{name}"))
}

/// `PUT` a binary to the board's upload endpoint.
pub async fn upload_binary(
    url: &str,
    token: Option<&str>,
    name: &str,
    bytes: &[u8],
) -> Result<Uploaded, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(UPLOAD_TIMEOUT_SECS))
        .build()
        .map_err(|err| format!("HTTP client: {err}"))?;

    let mut request = client
        .put(url)
        .header("Content-Type", "application/octet-stream")
        .body(bytes.to_vec());
    if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .await
        .map_err(|err| format!("upload to {url} failed: {err}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        let detail = body.trim();
        return Err(if detail.is_empty() {
            format!("board rejected the upload ({status})")
        } else {
            format!("board rejected the upload ({status}): {detail}")
        });
    }

    Ok(Uploaded {
        url: url.to_string(),
        name: name.to_string(),
        bytes: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A one-shot HTTP server: records the request and answers with `status`.
    fn one_shot_server(
        status: &'static str,
        body: &'static str,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buffer[..read]).to_string());

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (port, rx)
    }

    #[test]
    fn upload_endpoint_replaces_the_mcp_path() {
        assert_eq!(
            upload_endpoint("http://rpi5.local:8737/mcp").unwrap(),
            "http://rpi5.local:8737/upload"
        );
        assert_eq!(
            upload_endpoint("http://192.168.1.40:8737/").unwrap(),
            "http://192.168.1.40:8737/upload"
        );
        assert_eq!(
            upload_endpoint("http://192.168.1.40:8737").unwrap(),
            "http://192.168.1.40:8737/upload"
        );

        for bad in ["", "   ", "rpi5.local:8737/mcp", "http:///mcp"] {
            assert!(upload_endpoint(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn upload_url_rejects_separators_in_the_name() {
        assert_eq!(
            upload_url("http://rpi5.local:8737/mcp", "ai-traps-tests-rpi5").unwrap(),
            "http://rpi5.local:8737/upload/ai-traps-tests-rpi5"
        );

        assert!(upload_url("http://rpi5.local:8737/mcp", "").is_err());
        assert!(upload_url("http://rpi5.local:8737/mcp", "a/b").is_err());
        assert!(upload_url("http://rpi5.local:8737/mcp", "a\\b").is_err());
    }

    #[tokio::test]
    async fn upload_sends_the_binary_with_the_bearer_token() {
        let (port, captured) = one_shot_server("200 OK", r#"{"stored":"/tmp/smoke.sh"}"#);
        let url = format!("http://127.0.0.1:{port}/upload/smoke.sh");

        let uploaded = upload_binary(&url, Some("board-secret"), "smoke.sh", b"abc")
            .await
            .expect("uploaded");
        assert_eq!(uploaded.name, "smoke.sh");
        assert_eq!(uploaded.bytes, 3);

        let request = captured
            .recv_timeout(Duration::from_secs(5))
            .expect("request captured");
        assert!(
            request.starts_with("PUT /upload/smoke.sh HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer board-secret"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn upload_reports_a_rejected_upload() {
        let (port, _captured) = one_shot_server(
            "401 Unauthorized",
            r#"{"error":"missing or invalid bearer token"}"#,
        );
        let url = format!("http://127.0.0.1:{port}/upload/x");

        let err = upload_binary(&url, None, "x", b"abc")
            .await
            .expect_err("must fail");
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("bearer token"), "{err}");
    }
}
