use std::{
    future::Future,
    io::{ErrorKind, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::{Host, Url};
use uuid::Uuid;

use crate::config::IdpSection;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest a first sign-in can legitimately take: a failed refresh, discovery, the browser
/// round trip, and the code exchange. Tool hooks that kill a slow token command wait this long.
pub const SIGN_IN_CEILING: Duration =
    Duration::from_secs(HTTP_TIMEOUT.as_secs() * 3 + CALLBACK_TIMEOUT.as_secs());
const CALLBACK_PATH: &str = "/callback";
const DEFAULT_SCOPES: [&str; 4] = ["openid", "profile", "email", "offline_access"];
const FOREIGN_CALLBACK_MESSAGE: &str =
    "This response does not belong to the sign-in Relay is waiting for. Return to your terminal and use the link it printed.";

const PAGE_STYLE: &str = "<style>body{font-family:-apple-system,Segoe UI,Roboto,sans-serif;background:#0f172a;color:#e2e8f0;\
display:flex;min-height:100vh;align-items:center;justify-content:center;margin:0}\
.card{background:#1e293b;padding:36px 44px;border-radius:14px;text-align:center;max-width:520px}\
h1{font-size:20px;margin:0 0 6px}p{color:#94a3b8;margin:0}</style>";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    scopes_supported: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Callback {
    Code(String),
    Denied {
        error: String,
        description: Option<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Unrelated,
    Foreign,
    Callback(Callback),
}

/// Signs the developer in with OIDC authorization code plus PKCE against the
/// configured issuer, driving `open_browser` with the authorization URL and
/// completing the flow on a loopback redirect.
pub fn sign_in(idp: &IdpSection, open_browser: &dyn Fn(&str)) -> Result<Session> {
    sign_in_within(idp, open_browser, CALLBACK_TIMEOUT)
}

fn sign_in_within(
    idp: &IdpSection,
    open_browser: &dyn Fn(&str),
    callback_timeout: Duration,
) -> Result<Session> {
    let discovery = discover(idp)?;
    let scope = scope_string(idp, discovery.scopes_supported.as_deref());
    let port = idp.redirect_port.unwrap_or(0);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).with_context(|| {
        format!("failed to open the sign-in callback listener on 127.0.0.1:{port}")
    })?;
    listener
        .set_nonblocking(true)
        .context("failed to configure the sign-in callback listener")?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}{CALLBACK_PATH}",
        listener.local_addr()?.port()
    );
    let state = Uuid::new_v4().simple().to_string();
    let verifier = pkce_verifier();
    let url = authorization_url(
        &discovery.authorization_endpoint,
        idp,
        &redirect_uri,
        &scope,
        &state,
        &pkce_challenge(&verifier),
    )?;

    eprintln!("Opening your browser to sign in...");
    eprintln!("If it does not open, visit: {url}");
    open_browser(url.as_str());

    let (mut stream, callback) = await_callback(&listener, &state, callback_timeout)?;
    let outcome = redeem_callback(
        callback,
        &discovery.token_endpoint,
        idp,
        &redirect_uri,
        &verifier,
    );
    match &outcome {
        Ok(_) => respond(&mut stream, "200 OK", &success_page()),
        Err(error) => respond(&mut stream, "200 OK", &failure_page(&format!("{error:#}"))),
    }
    outcome
}

pub fn refresh(idp: &IdpSection, refresh_token: &str) -> Result<Session> {
    let discovery = discover(idp)?;
    let scope = scope_string(idp, discovery.scopes_supported.as_deref());
    let response = request_tokens(
        discovery.token_endpoint,
        vec![
            ("grant_type".into(), "refresh_token".into()),
            ("client_id".into(), idp.client_id.clone()),
            ("refresh_token".into(), refresh_token.to_string()),
            ("scope".into(), scope),
        ],
    )?;
    session_from(response, Some(refresh_token))
}

/// Reads the `exp` claim from a JWT without verifying the signature. Relay only
/// uses this to decide when a cached token needs to be refreshed; the Gateway
/// remains the sole authority that verifies the signature.
pub fn token_expiry(jwt: &str) -> Option<i64> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?;
    exp.as_i64()
        .or_else(|| exp.as_f64().map(|seconds| seconds.floor() as i64))
}

fn redeem_callback(
    callback: Callback,
    token_endpoint: &str,
    idp: &IdpSection,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Session> {
    let code = match callback {
        Callback::Code(code) => code,
        Callback::Denied { error, description } => bail!(
            "the IdP refused the sign-in: {error}{}",
            description
                .map(|text| format!(" ({text})"))
                .unwrap_or_default()
        ),
    };
    let response = request_tokens(
        token_endpoint.to_string(),
        vec![
            ("grant_type".into(), "authorization_code".into()),
            ("client_id".into(), idp.client_id.clone()),
            ("code".into(), code),
            ("redirect_uri".into(), redirect_uri.to_string()),
            ("code_verifier".into(), verifier.to_string()),
        ],
    )?;
    let session = session_from(response, None)?;
    if session.refresh_token.is_none() {
        eprintln!(
            "The IdP issued no refresh token; the next sign-in after this token expires needs the browser again."
        );
    }
    Ok(session)
}

fn session_from(response: TokenResponse, previous_refresh_token: Option<&str>) -> Result<Session> {
    let id_token = response
        .id_token
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "the IdP token response carries no id_token; the app registration must issue ID tokens for the openid scope"
            )
        })?;
    Ok(Session {
        id_token,
        refresh_token: response
            .refresh_token
            .filter(|token| !token.trim().is_empty())
            .or_else(|| previous_refresh_token.map(str::to_string)),
    })
}

fn discover(idp: &IdpSection) -> Result<Discovery> {
    let issuer = idp.normalized_issuer().to_string();
    let url = discovery_url(&issuer)?;
    let discovery = call(move || async move {
        let response = http_client()?
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to reach the IdP discovery document at {url}"))?;
        let status = response.status();
        if !status.is_success() {
            bail!("the IdP discovery document at {url} answered {status}");
        }
        response.json::<Discovery>().await.with_context(|| {
            format!("the IdP discovery document at {url} is not a valid OpenID configuration")
        })
    })?;
    validate_discovery(&issuer, discovery)
}

fn discovery_url(issuer: &str) -> Result<String> {
    if issuer.is_empty() {
        bail!("no IdP issuer configured");
    }
    require_secure(issuer, "issuer")?;
    Ok(format!("{issuer}/.well-known/openid-configuration"))
}

fn validate_discovery(issuer: &str, discovery: Discovery) -> Result<Discovery> {
    if discovery.issuer.trim_end_matches('/') != issuer {
        bail!(
            "the IdP discovery document names issuer {} but Relay is configured for {issuer}",
            discovery.issuer
        );
    }
    require_secure(&discovery.authorization_endpoint, "authorization endpoint")?;
    require_secure(&discovery.token_endpoint, "token endpoint")?;
    Ok(discovery)
}

fn require_secure(raw: &str, what: &str) -> Result<()> {
    let url = Url::parse(raw).with_context(|| format!("invalid IdP {what} URL: {raw}"))?;
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => domain == "localhost",
        None => false,
    };
    match (url.scheme(), loopback) {
        ("https", _) | ("http", true) => Ok(()),
        _ => bail!("the IdP {what} must use https: {raw}"),
    }
}

fn request_tokens(token_endpoint: String, form: Vec<(String, String)>) -> Result<TokenResponse> {
    call(move || async move {
        let response = http_client()?
            .post(&token_endpoint)
            .form(&form)
            .send()
            .await
            .with_context(|| format!("the IdP token request to {token_endpoint} failed"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read the IdP token response")?;
        let parsed: TokenResponse = serde_json::from_str(&body).with_context(|| {
            format!("the IdP token endpoint answered {status} with a non-JSON body")
        })?;
        if let Some(error) = parsed.error {
            bail!(
                "the IdP token endpoint answered {status}: {error}{}",
                parsed
                    .error_description
                    .map(|text| format!(" ({text})"))
                    .unwrap_or_default()
            );
        }
        if !status.is_success() {
            bail!("the IdP token endpoint answered {status}");
        }
        Ok(parsed)
    })
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("failed to build the IdP HTTP client")
}

fn call<T, F, Fut>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>>,
{
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to start the IdP HTTP runtime")?;
        runtime.block_on(work())
    });
    worker
        .join()
        .map_err(|_| anyhow!("the IdP HTTP worker panicked"))?
}

fn scope_string(idp: &IdpSection, supported: Option<&[String]>) -> String {
    match idp
        .scopes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(explicit) => ["openid"]
            .into_iter()
            .chain(
                explicit
                    .split_whitespace()
                    .filter(|scope| *scope != "openid"),
            )
            .collect::<Vec<_>>()
            .join(" "),
        None => default_scopes(supported).join(" "),
    }
}

fn default_scopes(supported: Option<&[String]>) -> Vec<&'static str> {
    DEFAULT_SCOPES
        .into_iter()
        .filter(|scope| {
            *scope == "openid" || supported.is_none_or(|list| list.iter().any(|s| s == scope))
        })
        .collect()
}

fn authorization_url(
    endpoint: &str,
    idp: &IdpSection,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    challenge: &str,
) -> Result<Url> {
    let mut url = Url::parse(endpoint)
        .with_context(|| format!("invalid IdP authorization endpoint: {endpoint}"))?;
    url.query_pairs_mut()
        .append_pair("client_id", &idp.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scope)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url)
}

fn pkce_verifier() -> String {
    format!(
        "{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn await_callback(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<(TcpStream, Callback)> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut stream = accept_before(listener, deadline)?;
        let Some(request_line) = read_request_line(&mut stream) else {
            continue;
        };
        match classify_request(&request_line, expected_state) {
            Request::Callback(callback) => return Ok((stream, callback)),
            Request::Unrelated => respond(&mut stream, "404 Not Found", ""),
            Request::Foreign => respond(
                &mut stream,
                "400 Bad Request",
                &failure_page(FOREIGN_CALLBACK_MESSAGE),
            ),
        }
    }
}

fn accept_before(listener: &TcpListener, deadline: Instant) -> Result<TcpStream> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("failed to configure the callback connection")?;
                stream.set_read_timeout(Some(CALLBACK_READ_TIMEOUT)).ok();
                return Ok(stream);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for the browser sign-in to complete");
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error).context("failed to accept the IdP sign-in callback"),
        }
    }
}

fn read_request_line(stream: &mut impl Read) -> Option<String> {
    let mut buffer = [0u8; 8192];
    let read = stream.read(&mut buffer).ok()?;
    String::from_utf8_lossy(&buffer[..read])
        .lines()
        .next()
        .map(str::to_string)
}

fn classify_request(request_line: &str, expected_state: &str) -> Request {
    let Some(target) = request_line.split_whitespace().nth(1) else {
        return Request::Unrelated;
    };
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Request::Unrelated;
    };
    if url.path() != CALLBACK_PATH {
        return Request::Unrelated;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if param("state").as_deref() != Some(expected_state) {
        return Request::Foreign;
    }
    match (param("code"), param("error")) {
        (Some(code), _) => Request::Callback(Callback::Code(code)),
        (None, Some(error)) => Request::Callback(Callback::Denied {
            error,
            description: param("error_description"),
        }),
        (None, None) => Request::Foreign,
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn success_page() -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Signed in</title>{PAGE_STYLE}</head>\
<body><div class=\"card\"><h1>You are signed in</h1>\
<p>Return to your terminal. You can close this tab.</p></div></body></html>"
    )
}

fn failure_page(message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sign-in failed</title>{PAGE_STYLE}</head>\
<body><div class=\"card\"><h1>Relay could not sign you in</h1><p>{}</p></div></body></html>",
        html_escape(message)
    )
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        collections::{HashMap, VecDeque},
        io::{ErrorKind, Read, Write},
        net::{Ipv4Addr, TcpListener, TcpStream},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    use url::Url;

    use crate::config::IdpSection;

    pub(crate) const CLIENT_ID: &str = "relay-test-client";

    #[derive(Default)]
    pub(crate) struct FakeIdpScript {
        pub scopes_supported: Option<Vec<String>>,
        pub token_replies: Vec<(u16, String)>,
        pub token_delay: Duration,
        pub discovery_reply: Option<(u16, String)>,
    }

    pub(crate) struct FakeIdp {
        pub issuer: String,
        token_requests: Arc<Mutex<Vec<HashMap<String, String>>>>,
        stop: Arc<AtomicBool>,
    }

    impl FakeIdp {
        pub(crate) fn start(
            scopes_supported: Option<&[&str]>,
            token_replies: Vec<(u16, String)>,
        ) -> Self {
            Self::start_with(FakeIdpScript {
                scopes_supported: scopes_supported
                    .map(|scopes| scopes.iter().map(|scope| scope.to_string()).collect()),
                token_replies,
                ..FakeIdpScript::default()
            })
        }

        pub(crate) fn start_with(script: FakeIdpScript) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let issuer = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
            let discovery = script.discovery_reply.unwrap_or_else(|| {
                (
                    200,
                    serde_json::json!({
                        "issuer": issuer,
                        "authorization_endpoint": format!("{issuer}/authorize"),
                        "token_endpoint": format!("{issuer}/token"),
                        "scopes_supported": script.scopes_supported,
                    })
                    .to_string(),
                )
            });
            let token_delay = script.token_delay;
            let replies = Arc::new(Mutex::new(VecDeque::from(script.token_replies)));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let server = FakeIdp {
                issuer,
                token_requests: requests.clone(),
                stop: stop.clone(),
            };
            thread::spawn(move || loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let (request_line, body) = read_request(&mut stream);
                        let reply = if request_line.contains("/.well-known/openid-configuration") {
                            discovery.clone()
                        } else if request_line.contains("/token") {
                            requests.lock().unwrap().push(parse_form(&body));
                            thread::sleep(token_delay);
                            replies
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or((500, "{\"error\":\"unscripted\"}".into()))
                        } else {
                            (404, "{}".into())
                        };
                        write_response(&mut stream, reply.0, &reply.1);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            });
            server
        }

        pub(crate) fn idp(&self) -> IdpSection {
            IdpSection {
                issuer: self.issuer.clone(),
                client_id: CLIENT_ID.into(),
                ..IdpSection::default()
            }
        }

        pub(crate) fn token_requests(&self) -> Vec<HashMap<String, String>> {
            self.token_requests.lock().unwrap().clone()
        }
    }

    impl Drop for FakeIdp {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn token_reply(id_token: &str, refresh_token: Option<&str>) -> (u16, String) {
        let mut body = serde_json::json!({
            "token_type": "Bearer",
            "expires_in": 3600,
            "access_token": "access-token-not-used",
            "id_token": id_token,
        });
        if let Some(refresh_token) = refresh_token {
            body["refresh_token"] = serde_json::Value::String(refresh_token.into());
        }
        (200, body.to_string())
    }

    pub(crate) fn error_reply(status: u16, error: &str, description: &str) -> (u16, String) {
        (
            status,
            serde_json::json!({ "error": error, "error_description": description }).to_string(),
        )
    }

    pub(crate) fn jwt_with_exp(exp: i64) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp},\"sub\":\"dev\"}}"));
        format!("{header}.{payload}.")
    }

    type Redirects = Arc<dyn Fn(&Url) -> Vec<String> + Send + Sync>;

    pub(crate) struct FakeBrowser {
        pub seen: Arc<Mutex<Vec<Url>>>,
        pages: Arc<Mutex<Vec<String>>>,
        redirects: Redirects,
    }

    impl FakeBrowser {
        pub(crate) fn approving() -> Self {
            Self::with_redirect(|url| {
                let state = query(url, "state").unwrap_or_default();
                format!("code=fake-code&state={state}")
            })
        }

        pub(crate) fn with_redirect(
            redirect: impl Fn(&Url) -> String + Send + Sync + 'static,
        ) -> Self {
            Self::with_redirects(move |url| vec![redirect(url)])
        }

        pub(crate) fn with_redirects(
            redirects: impl Fn(&Url) -> Vec<String> + Send + Sync + 'static,
        ) -> Self {
            FakeBrowser {
                seen: Arc::new(Mutex::new(Vec::new())),
                pages: Arc::new(Mutex::new(Vec::new())),
                redirects: Arc::new(redirects),
            }
        }

        pub(crate) fn opener(&self) -> impl Fn(&str) + '_ {
            move |raw| {
                let url = Url::parse(raw).unwrap();
                self.seen.lock().unwrap().push(url.clone());
                let redirect_uri = query(&url, "redirect_uri").unwrap();
                let targets: Vec<String> = (self.redirects)(&url)
                    .into_iter()
                    .map(|query| format!("{redirect_uri}?{query}"))
                    .collect();
                let pages = self.pages.clone();
                thread::spawn(move || {
                    http_get(&format!("{}/favicon.ico", origin(&redirect_uri)));
                    for target in targets {
                        let page = http_get(&target);
                        pages.lock().unwrap().push(page);
                    }
                });
            }
        }

        pub(crate) fn last_seen(&self) -> Url {
            self.seen.lock().unwrap().last().cloned().unwrap()
        }

        pub(crate) fn pages(&self, expected: usize) -> Vec<String> {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.pages.lock().unwrap().len() < expected && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            self.pages.lock().unwrap().clone()
        }
    }

    pub(crate) fn query(url: &Url, name: &str) -> Option<String> {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    fn origin(url: &str) -> String {
        let parsed = Url::parse(url).unwrap();
        format!(
            "http://{}:{}",
            parsed.host_str().unwrap(),
            parsed.port().unwrap()
        )
    }

    pub(crate) fn http_get(url: &str) -> String {
        let parsed = Url::parse(url).unwrap();
        let mut stream =
            TcpStream::connect((parsed.host_str().unwrap(), parsed.port().unwrap())).unwrap();
        let path = match parsed.query() {
            Some(query) => format!("{}?{query}", parsed.path()),
            None => parsed.path().to_string(),
        };
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    fn read_request(stream: &mut TcpStream) -> (String, String) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break raw.len();
            }
            raw.extend_from_slice(&chunk[..read]);
            if let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
        let content_length = head
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            })
            .unwrap_or(0);
        while raw.len() < header_end + content_length {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..read]);
        }
        let body = String::from_utf8_lossy(&raw[header_end..]).to_string();
        (head.lines().next().unwrap_or_default().to_string(), body)
    }

    fn parse_form(body: &str) -> HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "Error",
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let _ = stream.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::*, *};

    const STATE: &str = "expected-state";

    fn sign_in_quickly(idp: &IdpSection, open_browser: &dyn Fn(&str)) -> Result<Session> {
        sign_in_within(idp, open_browser, Duration::from_millis(400))
    }

    #[test]
    fn should_derive_the_pkce_challenge_per_rfc_7636() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn should_generate_a_fresh_high_entropy_verifier_each_time() {
        let first = pkce_verifier();
        let second = pkce_verifier();
        assert_ne!(first, second);
        assert!((64..=128).contains(&first.len()), "{}", first.len());
        assert!(first.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn should_accept_an_authorization_code_callback_for_the_expected_state() {
        assert_eq!(
            classify_request(
                "GET /callback?code=abc&state=expected-state HTTP/1.1",
                STATE
            ),
            Request::Callback(Callback::Code("abc".into()))
        );
    }

    #[test]
    fn should_accept_a_denied_callback_for_the_expected_state() {
        assert_eq!(
            classify_request(
                "GET /callback?error=access_denied&error_description=User%20declined&state=expected-state HTTP/1.1",
                STATE
            ),
            Request::Callback(Callback::Denied {
                error: "access_denied".into(),
                description: Some("User declined".into())
            })
        );
    }

    #[test]
    fn should_treat_requests_off_the_callback_path_as_unrelated() {
        assert_eq!(
            classify_request("GET /favicon.ico HTTP/1.1", STATE),
            Request::Unrelated
        );
        assert_eq!(
            classify_request(
                "GET /callback/../other?code=abc&state=expected-state HTTP/1.1",
                STATE
            ),
            Request::Unrelated
        );
        assert_eq!(classify_request("garbage", STATE), Request::Unrelated);
        assert_eq!(classify_request("", STATE), Request::Unrelated);
    }

    #[test]
    fn should_treat_a_callback_with_the_wrong_or_missing_state_as_foreign() {
        assert_eq!(
            classify_request("GET /callback?code=abc&state=forged HTTP/1.1", STATE),
            Request::Foreign
        );
        assert_eq!(
            classify_request("GET /callback?code=abc HTTP/1.1", STATE),
            Request::Foreign
        );
        assert_eq!(
            classify_request("GET /callback?error=access_denied HTTP/1.1", STATE),
            Request::Foreign
        );
        assert_eq!(
            classify_request("GET /callback?state=expected-state HTTP/1.1", STATE),
            Request::Foreign
        );
    }

    #[test]
    fn should_build_the_discovery_url_from_the_issuer() {
        assert_eq!(
            discovery_url("https://login.example.com/tenant/v2.0").unwrap(),
            "https://login.example.com/tenant/v2.0/.well-known/openid-configuration"
        );
        assert!(discovery_url("").is_err());
        assert!(discovery_url("not a url").is_err());
    }

    #[test]
    fn should_only_accept_https_issuers_off_the_loopback() {
        assert!(discovery_url("http://login.example.com").is_err());
        assert!(discovery_url("http://127.0.0.1:8080").is_ok());
        assert!(discovery_url("http://localhost:8080/realms/dev").is_ok());
        assert!(discovery_url("http://[::1]:8080").is_ok());
        assert!(discovery_url("ftp://login.example.com").is_err());
    }

    #[test]
    fn should_refuse_a_discovery_document_for_another_issuer() {
        let discovery = Discovery {
            issuer: "https://other.example.com".into(),
            authorization_endpoint: "https://login.example.com/authorize".into(),
            token_endpoint: "https://login.example.com/token".into(),
            scopes_supported: None,
        };

        let error = validate_discovery("https://login.example.com", discovery).unwrap_err();

        assert!(error.to_string().contains("other.example.com"), "{error:#}");
    }

    #[test]
    fn should_accept_a_discovery_document_whose_issuer_differs_only_by_a_trailing_slash() {
        let discovery = Discovery {
            issuer: "https://login.example.com/".into(),
            authorization_endpoint: "https://login.example.com/authorize".into(),
            token_endpoint: "https://login.example.com/token".into(),
            scopes_supported: None,
        };

        assert!(validate_discovery("https://login.example.com", discovery).is_ok());
    }

    #[test]
    fn should_refuse_plain_http_endpoints_in_the_discovery_document() {
        let insecure_token = Discovery {
            issuer: "https://login.example.com".into(),
            authorization_endpoint: "https://login.example.com/authorize".into(),
            token_endpoint: "http://login.example.com/token".into(),
            scopes_supported: None,
        };
        let error = validate_discovery("https://login.example.com", insecure_token).unwrap_err();
        assert!(error.to_string().contains("token endpoint"), "{error:#}");

        let insecure_authorize = Discovery {
            issuer: "https://login.example.com".into(),
            authorization_endpoint: "http://login.example.com/authorize".into(),
            token_endpoint: "https://login.example.com/token".into(),
            scopes_supported: None,
        };
        let error =
            validate_discovery("https://login.example.com", insecure_authorize).unwrap_err();
        assert!(
            error.to_string().contains("authorization endpoint"),
            "{error:#}"
        );
    }

    #[test]
    fn should_fail_clearly_when_the_discovery_document_is_missing_or_broken() {
        let missing = FakeIdp::start_with(FakeIdpScript {
            discovery_reply: Some((404, "{}".into())),
            ..FakeIdpScript::default()
        });
        let error = discover(&missing.idp()).unwrap_err();
        assert!(error.to_string().contains("answered 404"), "{error:#}");

        let broken = FakeIdp::start_with(FakeIdpScript {
            discovery_reply: Some((200, "<html>not json</html>".into())),
            ..FakeIdpScript::default()
        });
        let error = discover(&broken.idp()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not a valid OpenID configuration"),
            "{error:#}"
        );
    }

    #[test]
    fn should_refuse_to_sign_in_against_a_discovery_document_for_another_issuer() {
        let server = FakeIdp::start_with(FakeIdpScript {
            discovery_reply: Some((
                200,
                serde_json::json!({
                    "issuer": "https://other.example.com",
                    "authorization_endpoint": "https://other.example.com/authorize",
                    "token_endpoint": "https://other.example.com/token",
                })
                .to_string(),
            )),
            ..FakeIdpScript::default()
        });
        let browser = FakeBrowser::approving();

        let error = sign_in_quickly(&server.idp(), &browser.opener()).unwrap_err();

        assert!(error.to_string().contains("names issuer"), "{error:#}");
        assert!(browser.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn should_trim_default_scopes_to_what_the_idp_advertises() {
        let supported = vec!["openid".to_string(), "email".to_string()];
        assert_eq!(default_scopes(Some(&supported)), vec!["openid", "email"]);
        assert_eq!(
            default_scopes(None),
            vec!["openid", "profile", "email", "offline_access"]
        );
        assert_eq!(default_scopes(Some(&[])), vec!["openid"]);
    }

    #[test]
    fn should_send_explicit_scopes_with_openid_always_present() {
        let supported = vec!["openid".to_string()];
        let explicit = IdpSection {
            scopes: Some("  openid   api://gateway/access ".into()),
            ..IdpSection::default()
        };
        assert_eq!(
            scope_string(&explicit, Some(&supported)),
            "openid api://gateway/access"
        );

        let without_openid = IdpSection {
            scopes: Some("api://gateway/access offline_access".into()),
            ..IdpSection::default()
        };
        assert_eq!(
            scope_string(&without_openid, Some(&supported)),
            "openid api://gateway/access offline_access"
        );

        let blank = IdpSection {
            scopes: Some("   ".into()),
            ..IdpSection::default()
        };
        assert_eq!(scope_string(&blank, Some(&supported)), "openid");
    }

    #[test]
    fn should_read_exp_from_unverified_jwt() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJleHAiOjE4OTM0NTYwMDB9.";
        assert_eq!(token_expiry(jwt), Some(1_893_456_000));
    }

    #[test]
    fn should_read_a_fractional_exp_and_a_padded_payload() {
        let padded = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}"),
            base64::engine::general_purpose::URL_SAFE.encode(b"{\"exp\":1893456000.9}")
        );
        assert_eq!(token_expiry(&padded), Some(1_893_456_000));
    }

    #[test]
    fn should_return_none_for_token_without_exp_or_without_a_payload() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJhIn0.";
        assert_eq!(token_expiry(jwt), None);
        assert_eq!(token_expiry("not-a-jwt"), None);
        assert_eq!(token_expiry("a.!!!.c"), None);
        assert_eq!(token_expiry("a.bm90IGpzb24.c"), None);
    }

    #[test]
    fn should_complete_the_authorization_code_flow_with_pkce() {
        let server = FakeIdp::start(
            Some(&["openid", "profile", "offline_access"]),
            vec![token_reply("id-token-1", Some("refresh-1"))],
        );
        let browser = FakeBrowser::approving();

        let session = sign_in(&server.idp(), &browser.opener()).unwrap();

        assert_eq!(
            session,
            Session {
                id_token: "id-token-1".into(),
                refresh_token: Some("refresh-1".into())
            }
        );
        let authorize = browser.last_seen();
        assert!(authorize
            .as_str()
            .starts_with(&format!("{}/authorize?", server.issuer)));
        assert_eq!(query(&authorize, "response_type").as_deref(), Some("code"));
        assert_eq!(query(&authorize, "client_id").as_deref(), Some(CLIENT_ID));
        assert_eq!(
            query(&authorize, "scope").as_deref(),
            Some("openid profile offline_access")
        );
        assert_eq!(
            query(&authorize, "code_challenge_method").as_deref(),
            Some("S256")
        );
        let redirect_uri = query(&authorize, "redirect_uri").unwrap();
        assert!(redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(redirect_uri.ends_with("/callback"));

        let requests = server.token_requests();
        assert_eq!(requests.len(), 1);
        let exchange = &requests[0];
        assert_eq!(exchange["grant_type"], "authorization_code");
        assert_eq!(exchange["code"], "fake-code");
        assert_eq!(exchange["client_id"], CLIENT_ID);
        assert_eq!(exchange["redirect_uri"], redirect_uri);
        assert!(!exchange.contains_key("client_secret"));
        assert_eq!(
            pkce_challenge(&exchange["code_verifier"]),
            query(&authorize, "code_challenge").unwrap()
        );
        let pages = browser.pages(1);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].starts_with("HTTP/1.1 200 OK"), "{}", pages[0]);
        assert!(pages[0].contains("You are signed in"), "{}", pages[0]);
    }

    #[test]
    fn should_use_the_configured_redirect_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let server = FakeIdp::start(None, vec![token_reply("id-token", None)]);
        let idp = IdpSection {
            redirect_port: Some(port),
            ..server.idp()
        };
        let browser = FakeBrowser::approving();

        sign_in(&idp, &browser.opener()).unwrap();

        assert_eq!(
            query(&browser.last_seen(), "redirect_uri").unwrap(),
            format!("http://127.0.0.1:{port}/callback")
        );
    }

    #[test]
    fn should_keep_waiting_past_a_callback_whose_state_does_not_match() {
        let server = FakeIdp::start(None, vec![token_reply("id-token", None)]);
        let browser = FakeBrowser::with_redirects(|url| {
            vec![
                "code=forged-code&state=forged".into(),
                "code=forged-code".into(),
                format!("code=real-code&state={}", query(url, "state").unwrap()),
            ]
        });

        let session = sign_in(&server.idp(), &browser.opener()).unwrap();

        assert_eq!(session.id_token, "id-token");
        let requests = server.token_requests();
        assert_eq!(
            requests.len(),
            1,
            "a forged callback must never be redeemed"
        );
        assert_eq!(requests[0]["code"], "real-code");
        let pages = browser.pages(3);
        assert!(
            pages[0].starts_with("HTTP/1.1 400 Bad Request"),
            "{}",
            pages[0]
        );
        assert!(
            pages[0].contains("does not belong to the sign-in"),
            "{}",
            pages[0]
        );
        assert!(
            pages[1].starts_with("HTTP/1.1 400 Bad Request"),
            "{}",
            pages[1]
        );
        assert!(pages[2].starts_with("HTTP/1.1 200 OK"), "{}", pages[2]);
    }

    #[test]
    fn should_time_out_when_only_forged_callbacks_arrive() {
        let server = FakeIdp::start(None, vec![token_reply("id-token", None)]);
        let browser = FakeBrowser::with_redirect(|_| "code=forged-code&state=forged".into());

        let error = sign_in_quickly(&server.idp(), &browser.opener()).unwrap_err();

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(server.token_requests().is_empty());
    }

    #[test]
    fn should_keep_waiting_past_an_idle_connection() {
        let server = FakeIdp::start(None, vec![token_reply("id-token", None)]);
        let browser = FakeBrowser::with_redirect(|url| {
            let redirect_uri = query(url, "redirect_uri").unwrap();
            let parsed = Url::parse(&redirect_uri).unwrap();
            let idle =
                TcpStream::connect((parsed.host_str().unwrap(), parsed.port().unwrap())).unwrap();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(4));
                drop(idle);
            });
            format!("code=fake-code&state={}", query(url, "state").unwrap())
        });

        let started = Instant::now();
        let session = sign_in(&server.idp(), &browser.opener()).unwrap();

        assert_eq!(session.id_token, "id-token");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn should_surface_a_denied_sign_in() {
        let server = FakeIdp::start(None, vec![]);
        let browser = FakeBrowser::with_redirect(|url| {
            format!(
                "error=access_denied&error_description=consent_required%20%3Cb%3E&state={}",
                query(url, "state").unwrap()
            )
        });

        let error = sign_in(&server.idp(), &browser.opener()).unwrap_err();

        assert!(error.to_string().contains("access_denied"), "{error:#}");
        assert!(error.to_string().contains("consent_required"), "{error:#}");
        assert!(server.token_requests().is_empty());
        let pages = browser.pages(1);
        assert!(
            pages[0].contains("Relay could not sign you in"),
            "{}",
            pages[0]
        );
        assert!(
            pages[0].contains("consent_required &lt;b&gt;"),
            "{}",
            pages[0]
        );
        assert!(!pages[0].contains("<b>"), "{}", pages[0]);
    }

    #[test]
    fn should_fail_when_the_token_response_has_no_id_token() {
        let server = FakeIdp::start(
            None,
            vec![(
                200,
                "{\"access_token\":\"only-an-access-token\",\"id_token\":\" \"}".into(),
            )],
        );
        let browser = FakeBrowser::approving();

        let error = sign_in(&server.idp(), &browser.opener()).unwrap_err();

        assert!(error.to_string().contains("no id_token"), "{error:#}");
        let pages = browser.pages(1);
        assert!(pages[0].contains("no id_token"), "{}", pages[0]);
    }

    #[test]
    fn should_renew_with_the_refresh_token_grant() {
        let server = FakeIdp::start(None, vec![token_reply("id-token-2", Some("refresh-2"))]);

        let session = refresh(&server.idp(), "refresh-1").unwrap();

        assert_eq!(
            session,
            Session {
                id_token: "id-token-2".into(),
                refresh_token: Some("refresh-2".into())
            }
        );
        let requests = server.token_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["grant_type"], "refresh_token");
        assert_eq!(requests[0]["refresh_token"], "refresh-1");
        assert_eq!(requests[0]["client_id"], CLIENT_ID);
        assert_eq!(requests[0]["scope"], "openid profile email offline_access");
    }

    #[test]
    fn should_keep_the_previous_refresh_token_when_the_idp_does_not_rotate_it() {
        let server = FakeIdp::start(
            None,
            vec![
                token_reply("id-token-2", None),
                token_reply("id-token-3", Some("")),
            ],
        );

        let session = refresh(&server.idp(), "refresh-1").unwrap();
        assert_eq!(session.refresh_token.as_deref(), Some("refresh-1"));

        let session = refresh(&server.idp(), "refresh-1").unwrap();
        assert_eq!(session.refresh_token.as_deref(), Some("refresh-1"));
    }

    #[test]
    fn should_report_a_rejected_refresh() {
        let server = FakeIdp::start(
            None,
            vec![error_reply(
                400,
                "invalid_grant",
                "AADSTS70008: refresh token expired",
            )],
        );

        let error = refresh(&server.idp(), "refresh-1").unwrap_err();

        assert!(error.to_string().contains("invalid_grant"), "{error:#}");
        assert!(error.to_string().contains("AADSTS70008"), "{error:#}");
    }

    #[test]
    fn should_report_a_non_json_token_response() {
        let server = FakeIdp::start(None, vec![(502, "<html>bad gateway</html>".into())]);

        let error = refresh(&server.idp(), "refresh-1").unwrap_err();

        assert!(error.to_string().contains("non-JSON body"), "{error:#}");
        assert!(error.to_string().contains("502"), "{error:#}");
    }
}
