use std::{
    io::{ErrorKind, Read, Write},
    net::{Ipv4Addr, TcpListener},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use url::Url;
use uuid::Uuid;

use crate::auth::open_browser;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

const SUCCESS_PAGE: &str =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Signed in</title>\
<style>body{font-family:-apple-system,Segoe UI,Roboto,sans-serif;background:#0f172a;color:#e2e8f0;\
display:flex;min-height:100vh;align-items:center;justify-content:center;margin:0}\
.card{background:#1e293b;padding:36px 44px;border-radius:14px;text-align:center}\
h1{font-size:20px;margin:0 0 6px}p{color:#94a3b8;margin:0}</style></head>\
<body><div class=\"card\"><h1>You are signed in</h1>\
<p>Return to your terminal. You can close this tab.</p></div></body></html>";

/// Signs the developer into the corporate IdP through a browser and returns the
/// resulting bearer token. Relay opens the IdP authorize URL with a loopback
/// redirect, then reads the token the IdP hands back to the local callback.
pub fn sign_in(authorize_url: &str) -> Result<String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .context("failed to open a local callback listener for IdP sign-in")?;
    listener
        .set_nonblocking(true)
        .context("failed to configure the local callback listener")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let state = Uuid::new_v4().to_string();

    let mut url = Url::parse(authorize_url)
        .with_context(|| format!("invalid IdP authorize URL: {authorize_url}"))?;
    url.query_pairs_mut()
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .append_pair("response_type", "token");

    eprintln!("Opening your browser to sign in...");
    eprintln!("If it does not open, visit: {url}");
    open_browser(url.as_str());

    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for the browser sign-in to complete");
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(error).context("failed to accept the IdP sign-in callback");
            }
        }
    };
    stream
        .set_nonblocking(false)
        .context("failed to configure the callback connection")?;
    stream.set_read_timeout(Some(CALLBACK_TIMEOUT)).ok();

    let request_line = read_request_line(&mut stream)?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        SUCCESS_PAGE.len(),
        SUCCESS_PAGE
    );
    let _ = stream.write_all(response.as_bytes());

    let (token, returned_state) = parse_callback(&request_line)?;
    if returned_state != state {
        bail!("IdP sign-in state mismatch; aborting");
    }
    if token.trim().is_empty() {
        bail!("IdP sign-in did not return a token");
    }
    Ok(token)
}

fn read_request_line(stream: &mut impl Read) -> Result<String> {
    let mut buffer = [0u8; 8192];
    let read = stream
        .read(&mut buffer)
        .context("failed to read the IdP callback request")?;
    let text = String::from_utf8_lossy(&buffer[..read]);
    text.lines()
        .next()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("empty IdP callback request"))
}

fn parse_callback(request_line: &str) -> Result<(String, String)> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed IdP callback request line"))?;
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .context("failed to parse the IdP callback URL")?;
    let mut token = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "token" => token = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    Ok((
        token.ok_or_else(|| anyhow!("IdP callback did not include a token"))?,
        state.unwrap_or_default(),
    ))
}

/// Reads the `exp` claim from a JWT without verifying the signature. Relay only
/// uses this to decide when a cached token needs to be refreshed; the Gateway
/// remains the sole authority that verifies the signature.
pub fn token_expiry(jwt: &str) -> Option<i64> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp")?.as_i64()
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut lookup = [255u8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }

    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return None;
        }
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((accumulator >> bits) as u8);
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_extract_token_and_state_from_callback() {
        let (token, state) =
            parse_callback("GET /callback?token=abc.def.ghi&state=xyz HTTP/1.1").unwrap();
        assert_eq!(token, "abc.def.ghi");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn should_fail_callback_without_token() {
        assert!(parse_callback("GET /callback?state=xyz HTTP/1.1").is_err());
    }

    #[test]
    fn should_read_exp_from_unverified_jwt() {
        // header {"alg":"none"} . payload {"exp":1893456000} . (no signature)
        let jwt = "eyJhbGciOiJub25lIn0.eyJleHAiOjE4OTM0NTYwMDB9.";
        assert_eq!(token_expiry(jwt), Some(1_893_456_000));
    }

    #[test]
    fn should_return_none_for_token_without_exp() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJhIn0.";
        assert_eq!(token_expiry(jwt), None);
    }
}
