//! The Gateway credential every onboarded tool sends as its bearer: Relay
//! exchanges the developer's IdP token for it (RFC 8693) at the Gateway's
//! authorization server and renews it with the rotating refresh token.

use std::{error::Error, fmt, fs, path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::{
    ai_tools::{
        blocking::call,
        token::{ensure_token, SignIn},
    },
    config::{relay_home, RelaySettings},
    system::{lock_private, write_private},
};

const CACHE_FILE: &str = "gateway-credentials.json";
const LOCK_FILE: &str = "gateway-credentials.lock";
const DISCOVERY_PATH: &str = "/.well-known/litellm-cli-auth";
const TOKEN_EXCHANGE_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const JWT_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:jwt";
const CLIENT_NAME: &str = "litellm-relay";
const REGISTERED_REDIRECT_URI: &str = "http://127.0.0.1/litellm-relay/callback";
const TEAM_HEADER: &str = "x-litellm-team-id";
const HTTP_TIMEOUT_SECONDS: u64 = 30;
const HTTP_CONNECT_TIMEOUT_SECONDS: u64 = 10;

// Claude Code and Codex cache the helper's output for five minutes, so a token
// they fetched right before a renewal has to stay valid until they ask again.
const CREDENTIAL_REFRESH_SKEW_SECONDS: i64 = 600;
const ASSUMED_CREDENTIAL_LIFETIME_SECONDS: i64 = 3600;

/// When a cached credential is renewed. Token hooks run every few minutes, so
/// they renew near expiry. A file an app reads once at launch is renewed on
/// every run, so the key it holds always has close to a full lifetime left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    NearExpiry,
    HalfLife,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayCredential {
    /// The access token to send as the bearer.
    Issued(String),
    /// The Gateway exposes no authorization server with the token-exchange
    /// grant, so no Gateway credential can be obtained from the IdP token.
    Unsupported,
}

/// The Gateway's authorization server answered the exchange with a 4xx, so the
/// grant itself was rejected rather than the request failing on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeRefused(String);

impl fmt::Display for ExchangeRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for ExchangeRefused {}

/// The IdP sign-in the exchange needed did not produce an identity token.
/// Transparent: it reads exactly like the sign-in error it carries.
#[derive(Debug)]
pub struct SignInFailed(anyhow::Error);

impl fmt::Display for SignInFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Error for SignInFailed {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperBearer {
    Credential(String),
    IdentityToken { token: String, notice: String },
}

/// Returns a fresh Gateway credential for the tool's team, exchanging or
/// refreshing as needed. The cache under `~/.litellm-relay/` is shared across
/// tools and keyed by Gateway URL and team, so tools on different teams keep
/// their own credential.
pub fn ensure_gateway_credential(
    settings: &RelaySettings,
    team: Option<&str>,
    sign_in: SignIn,
    renewal: Renewal,
) -> Result<GatewayCredential> {
    let gateway_url = settings.gateway.url.trim_end_matches('/').to_string();
    let mut identity_token = || ensure_token(&settings.idp, sign_in);
    let request = CredentialRequest {
        gateway_url: &gateway_url,
        team,
        renewal,
    };
    let unlocked = load_store()?;
    if let Some(credential) = fresh(&unlocked.credentials, request, Utc::now().timestamp()) {
        return Ok(GatewayCredential::Issued(credential.access_token.clone()));
    }
    let _renewing = lock_store()?;
    let store = load_store()?;
    let resolved = resolve(
        &HttpAuthorizationServer,
        &mut identity_token,
        &store.credentials,
        request,
        Utc::now().timestamp(),
    )?;
    match resolved {
        Resolved::Unsupported => Ok(GatewayCredential::Unsupported),
        Resolved::Issued {
            credential,
            changed,
        } => {
            if changed {
                save_store(&CredentialStore {
                    credentials: upsert(&store.credentials, credential.clone()),
                })?;
            }
            Ok(GatewayCredential::Issued(credential.access_token))
        }
    }
}

/// Prints the bearer a tool's token hook should use: the Gateway credential,
/// or the raw identity token (with a notice on stderr) whenever the Gateway
/// could not issue one, which is what the hooks sent before the exchange
/// existed. Only a refused exchange and a failed sign-in stay errors.
pub fn print_bearer(settings: &RelaySettings, team: Option<&str>) -> Result<()> {
    let resolved = ensure_gateway_credential(settings, team, SignIn::Allowed, Renewal::NearExpiry);
    let identity_token = || ensure_token(&settings.idp, SignIn::Allowed);
    let bearer = match helper_bearer(&settings.gateway.url, resolved, identity_token)? {
        HelperBearer::Credential(token) => token,
        HelperBearer::IdentityToken { token, notice } => {
            eprintln!("{notice}");
            token
        }
    };
    println!("{bearer}");
    Ok(())
}

pub fn helper_bearer(
    gateway_url: &str,
    resolved: Result<GatewayCredential>,
    identity_token: impl FnOnce() -> Result<String>,
) -> Result<HelperBearer> {
    let notice = match resolved {
        Ok(GatewayCredential::Issued(token)) => return Ok(HelperBearer::Credential(token)),
        Ok(GatewayCredential::Unsupported) => format!(
            "gateway {gateway_url} offers no IdP token exchange; sending the identity token instead"
        ),
        Err(error) if error.is::<ExchangeRefused>() || error.is::<SignInFailed>() => {
            return Err(error)
        }
        Err(error) => format!(
            "gateway {gateway_url} could not issue a credential, sending the identity token \
             instead: {error:#}"
        ),
    };
    Ok(HelperBearer::IdentityToken {
        token: identity_token()?,
        notice,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialRequest<'a> {
    pub gateway_url: &'a str,
    pub team: Option<&'a str>,
    pub renewal: Renewal,
}

/// The parts of the Gateway's `/.well-known/litellm-cli-auth` document Relay
/// needs to register and to redeem grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationServerDocument {
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub resource: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    Supported(AuthorizationServerDocument),
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IssuedTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OAuthError {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenReply {
    Issued(IssuedTokens),
    /// Any 4xx answer: the Gateway rejected this grant. Transport failures and
    /// 5xx answers are errors instead, because they say nothing about the grant.
    Refused {
        status: u16,
        error: OAuthError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Exchange {
        client_id: String,
        subject_token: String,
        team: Option<String>,
    },
    Refresh {
        client_id: String,
        refresh_token: String,
    },
}

/// The Gateway's authorization server as Relay uses it. Injected so the
/// credential state machine is tested against a scripted server.
pub trait AuthorizationServer {
    fn discover(&self, gateway_url: &str) -> Result<Discovery>;
    fn register(&self, registration_endpoint: &str) -> Result<String>;
    fn token(&self, document: &AuthorizationServerDocument, grant: Grant) -> Result<TokenReply>;
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct CachedCredential {
    gateway_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team: Option<String>,
    client_id: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    expires_at: i64,
    #[serde(default)]
    issued_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_id: Option<String>,
}

impl CachedCredential {
    fn matches(&self, request: CredentialRequest<'_>) -> bool {
        self.gateway_url == request.gateway_url && self.team.as_deref() == request.team
    }

    fn is_fresh(&self, now: i64, renewal: Renewal) -> bool {
        match renewal {
            Renewal::NearExpiry => self.expires_at > now + CREDENTIAL_REFRESH_SKEW_SECONDS,
            Renewal::HalfLife => self.expires_at - now > (self.expires_at - self.issued_at) / 2,
        }
    }

    fn is_valid(&self, now: i64) -> bool {
        self.expires_at > now
    }

    fn issued(
        request: CredentialRequest<'_>,
        client_id: String,
        tokens: IssuedTokens,
        now: i64,
    ) -> Self {
        Self {
            gateway_url: request.gateway_url.to_string(),
            team: request.team.map(str::to_string),
            client_id,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: expires_at(tokens.expires_in, now),
            issued_at: now,
            user_id: tokens.user_id,
            team_id: tokens.team_id,
        }
    }

    fn renewed(&self, tokens: IssuedTokens, now: i64) -> Self {
        Self {
            gateway_url: self.gateway_url.clone(),
            team: self.team.clone(),
            client_id: self.client_id.clone(),
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token.or_else(|| self.refresh_token.clone()),
            expires_at: expires_at(tokens.expires_in, now),
            issued_at: now,
            user_id: tokens.user_id.or_else(|| self.user_id.clone()),
            team_id: tokens.team_id.or_else(|| self.team_id.clone()),
        }
    }
}

fn expires_at(expires_in: Option<i64>, now: i64) -> i64 {
    now + expires_in.unwrap_or(ASSUMED_CREDENTIAL_LIFETIME_SECONDS)
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CredentialStore {
    credentials: Vec<CachedCredential>,
}

#[derive(Debug, PartialEq, Eq)]
enum Resolved {
    Issued {
        credential: CachedCredential,
        changed: bool,
    },
    Unsupported,
}

enum ExchangeReply {
    Issued(IssuedTokens),
    UnknownClient,
}

fn fresh<'a>(
    store: &'a [CachedCredential],
    request: CredentialRequest<'_>,
    now: i64,
) -> Option<&'a CachedCredential> {
    store
        .iter()
        .find(|credential| credential.matches(request))
        .filter(|credential| credential.is_fresh(now, request.renewal))
}

fn resolve(
    server: &impl AuthorizationServer,
    identity_token: &mut dyn FnMut() -> Result<String>,
    store: &[CachedCredential],
    request: CredentialRequest<'_>,
    now: i64,
) -> Result<Resolved> {
    if let Some(credential) = fresh(store, request, now) {
        return Ok(Resolved::Issued {
            credential: credential.clone(),
            changed: false,
        });
    }
    let cached = store.iter().find(|credential| credential.matches(request));
    match renew(server, identity_token, cached, request, now) {
        Err(error) => reuse_while_valid(cached, now, error),
        renewed => renewed,
    }
}

fn renew(
    server: &impl AuthorizationServer,
    identity_token: &mut dyn FnMut() -> Result<String>,
    cached: Option<&CachedCredential>,
    request: CredentialRequest<'_>,
    now: i64,
) -> Result<Resolved> {
    let document = match server.discover(request.gateway_url)? {
        Discovery::Supported(document) => document,
        Discovery::Unsupported => return Ok(Resolved::Unsupported),
    };
    if let Some(credential) = cached {
        if let Some(tokens) = refresh(server, &document, credential)? {
            return Ok(Resolved::Issued {
                credential: credential.renewed(tokens, now),
                changed: true,
            });
        }
    }
    let subject_token =
        identity_token().map_err(|error| anyhow::Error::new(SignInFailed(error)))?;
    let known_client = cached.map(|credential| credential.client_id.as_str());
    let (client_id, tokens) = exchange(server, &document, &subject_token, request, known_client)?;
    Ok(Resolved::Issued {
        credential: CachedCredential::issued(request, client_id, tokens, now),
        changed: true,
    })
}

fn reuse_while_valid(
    cached: Option<&CachedCredential>,
    now: i64,
    error: anyhow::Error,
) -> Result<Resolved> {
    match cached.filter(|credential| credential.is_valid(now)) {
        Some(credential) => {
            eprintln!("gateway credential renewal failed, reusing the current one: {error:#}");
            Ok(Resolved::Issued {
                credential: credential.clone(),
                changed: false,
            })
        }
        None => Err(error),
    }
}

fn refresh(
    server: &impl AuthorizationServer,
    document: &AuthorizationServerDocument,
    credential: &CachedCredential,
) -> Result<Option<IssuedTokens>> {
    let Some(refresh_token) = credential.refresh_token.as_deref() else {
        return Ok(None);
    };
    let grant = Grant::Refresh {
        client_id: credential.client_id.clone(),
        refresh_token: refresh_token.to_string(),
    };
    match server.token(document, grant)? {
        TokenReply::Issued(tokens) => Ok(Some(tokens)),
        TokenReply::Refused { .. } => Ok(None),
    }
}

fn exchange(
    server: &impl AuthorizationServer,
    document: &AuthorizationServerDocument,
    subject_token: &str,
    request: CredentialRequest<'_>,
    known_client: Option<&str>,
) -> Result<(String, IssuedTokens)> {
    if let Some(client_id) = known_client {
        if let ExchangeReply::Issued(tokens) =
            attempt_exchange(server, document, client_id, subject_token, request.team)?
        {
            return Ok((client_id.to_string(), tokens));
        }
    }
    let client_id = server.register(&document.registration_endpoint)?;
    match attempt_exchange(server, document, &client_id, subject_token, request.team)? {
        ExchangeReply::Issued(tokens) => Ok((client_id, tokens)),
        ExchangeReply::UnknownClient => {
            bail!("the gateway does not recognize the client it just registered")
        }
    }
}

fn attempt_exchange(
    server: &impl AuthorizationServer,
    document: &AuthorizationServerDocument,
    client_id: &str,
    subject_token: &str,
    team: Option<&str>,
) -> Result<ExchangeReply> {
    let grant = Grant::Exchange {
        client_id: client_id.to_string(),
        subject_token: subject_token.to_string(),
        team: team.map(str::to_string),
    };
    match server.token(document, grant)? {
        TokenReply::Issued(tokens) => Ok(ExchangeReply::Issued(tokens)),
        TokenReply::Refused { error, .. } if error.error == "invalid_client" => {
            Ok(ExchangeReply::UnknownClient)
        }
        TokenReply::Refused { status, error } => Err(anyhow::Error::new(ExchangeRefused(format!(
            "the gateway refused the token exchange ({}){}",
            refusal_summary(status, &error),
            team_hint(team, &error.error)
        )))),
    }
}

fn refusal_summary(status: u16, error: &OAuthError) -> String {
    if error.error.is_empty() {
        return format!("HTTP {status}");
    }
    format!("HTTP {status} {}: {}", error.error, error.error_description)
}

fn team_hint(team: Option<&str>, refusal: &str) -> String {
    if refusal != "invalid_request" && refusal != "invalid_grant" {
        return String::new();
    }
    match team {
        Some(team) => format!(
            "; this tool is set to team `{team}`, so if the gateway does not accept that team \
             for your sign-in, onboard the tool again with a `--team <team>` it does accept"
        ),
        None => "; no team is set for this tool, so if you belong to a team on this gateway, \
                 onboard the tool again with `--team <team>`"
            .to_string(),
    }
}

fn upsert(store: &[CachedCredential], credential: CachedCredential) -> Vec<CachedCredential> {
    let replaced = (credential.gateway_url.clone(), credential.team.clone());
    store
        .iter()
        .filter(|existing| (existing.gateway_url.clone(), existing.team.clone()) != replaced)
        .cloned()
        .chain(std::iter::once(credential))
        .collect()
}

fn store_path() -> PathBuf {
    relay_home().join(CACHE_FILE)
}

fn load_store() -> Result<CredentialStore> {
    let path = store_path();
    if !path.exists() {
        return Ok(CredentialStore::default());
    }
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(decode_store(&contents))
}

fn decode_store(contents: &str) -> CredentialStore {
    serde_json::from_str(contents).unwrap_or_else(|error| {
        eprintln!("ignoring an unreadable gateway credential cache: {error}");
        CredentialStore::default()
    })
}

fn save_store(store: &CredentialStore) -> Result<()> {
    write_private(&store_path(), &serde_json::to_string(store)?)
}

// Concurrent token hooks take turns so one renewal serves all of them: the
// loser re-reads the cache and finds the winner's credential.
fn lock_store() -> Result<fs::File> {
    lock_private(&relay_home().join(LOCK_FILE))
}

/// The real authorization server over HTTP. Each call runs on a scratch thread
/// with its own runtime because the CLI's tokio runtime is already driving the
/// synchronous command that asked for the credential.
pub struct HttpAuthorizationServer;

struct Reply {
    status: u16,
    body: String,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    grant_types_supported: Vec<String>,
}

#[derive(Deserialize)]
struct RegistrationReply {
    client_id: String,
}

impl AuthorizationServer for HttpAuthorizationServer {
    fn discover(&self, gateway_url: &str) -> Result<Discovery> {
        let url = format!("{}{DISCOVERY_PATH}", gateway_url.trim_end_matches('/'));
        let reply =
            call(move || async move { fetch(http_client(Policy::default())?.get(&url)).await })?;
        parse_discovery(reply, gateway_url)
    }

    fn register(&self, registration_endpoint: &str) -> Result<String> {
        let endpoint = registration_endpoint.to_string();
        let body = json!({
            "client_name": CLIENT_NAME,
            "redirect_uris": [REGISTERED_REDIRECT_URI],
            "token_endpoint_auth_method": "none",
            "grant_types": [TOKEN_EXCHANGE_GRANT, "refresh_token"],
        });
        let reply = call(move || async move {
            fetch(http_client(Policy::none())?.post(&endpoint).json(&body)).await
        })?;
        parse_registration(reply)
    }

    fn token(&self, document: &AuthorizationServerDocument, grant: Grant) -> Result<TokenReply> {
        let endpoint = document.token_endpoint.clone();
        let (form, team) = token_form(document, grant);
        let reply = call(move || async move {
            let request = http_client(Policy::none())?.post(&endpoint).form(&form);
            let request = match team {
                Some(team) => request.header(TEAM_HEADER, team),
                None => request,
            };
            fetch(request).await
        })?;
        parse_token_reply(reply)
    }
}

fn token_form(
    document: &AuthorizationServerDocument,
    grant: Grant,
) -> (Vec<(&'static str, String)>, Option<String>) {
    let resource = document
        .resource
        .clone()
        .map(|resource| ("resource", resource));
    match grant {
        Grant::Exchange {
            client_id,
            subject_token,
            team,
        } => {
            let form = vec![
                ("grant_type", TOKEN_EXCHANGE_GRANT.to_string()),
                ("client_id", client_id),
                ("subject_token", subject_token),
                ("subject_token_type", JWT_TOKEN_TYPE.to_string()),
            ];
            (form.into_iter().chain(resource).collect(), team)
        }
        Grant::Refresh {
            client_id,
            refresh_token,
        } => {
            let form = vec![
                ("grant_type", "refresh_token".to_string()),
                ("client_id", client_id),
                ("refresh_token", refresh_token),
            ];
            (form.into_iter().chain(resource).collect(), None)
        }
    }
}

fn parse_discovery(reply: Reply, gateway_url: &str) -> Result<Discovery> {
    match reply.status {
        404 => Ok(Discovery::Unsupported),
        200 => {
            let document: DiscoveryDocument = serde_json::from_str(&reply.body)
                .context("the gateway's authorization server document is not valid JSON")?;
            if !document
                .grant_types_supported
                .iter()
                .any(|grant| grant == TOKEN_EXCHANGE_GRANT)
            {
                return Ok(Discovery::Unsupported);
            }
            let registration_endpoint = document.registration_endpoint.ok_or_else(|| {
                anyhow!("the gateway's authorization server offers no registration endpoint")
            })?;
            Ok(Discovery::Supported(AuthorizationServerDocument {
                token_endpoint: on_gateway_origin(gateway_url, &document.token_endpoint)?,
                registration_endpoint: on_gateway_origin(gateway_url, &registration_endpoint)?,
                resource: document.resource,
            }))
        }
        status => bail!("gateway discovery answered HTTP {status}"),
    }
}

// The identity token only ever goes to the origin the operator configured. A
// Gateway behind a TLS-terminating proxy advertises its internal `http://`
// address, and a document naming another host must not receive the token.
fn on_gateway_origin(gateway_url: &str, advertised: &str) -> Result<String> {
    let mut endpoint = Url::parse(gateway_url)
        .with_context(|| format!("the gateway URL {gateway_url} is not a valid URL"))?;
    let advertised = Url::parse(advertised)
        .with_context(|| format!("the gateway advertised an invalid endpoint {advertised}"))?;
    endpoint.set_path(advertised.path());
    endpoint.set_query(advertised.query());
    Ok(endpoint.into())
}

fn parse_registration(reply: Reply) -> Result<String> {
    match reply.status {
        200 | 201 => {
            let registration: RegistrationReply = serde_json::from_str(&reply.body)
                .context("the gateway's registration reply is not valid JSON")?;
            Ok(registration.client_id)
        }
        status => bail!(
            "gateway client registration failed (HTTP {status}): {}",
            oauth_error_summary(&reply.body)
        ),
    }
}

fn parse_token_reply(reply: Reply) -> Result<TokenReply> {
    match reply.status {
        200 => {
            let tokens: IssuedTokens = serde_json::from_str(&reply.body)
                .context("the gateway's token reply is not valid JSON")?;
            Ok(TokenReply::Issued(tokens))
        }
        status @ 400..=499 => Ok(TokenReply::Refused {
            status,
            error: serde_json::from_str(&reply.body).unwrap_or_default(),
        }),
        status @ 300..=399 => bail!(
            "the gateway redirected the token request (HTTP {status}); point gateway.url at \
             the address it redirects to"
        ),
        status => bail!(
            "gateway token request failed (HTTP {status}): {}",
            oauth_error_summary(&reply.body)
        ),
    }
}

fn oauth_error_summary(body: &str) -> String {
    serde_json::from_str::<OAuthError>(body)
        .map(|error| format!("{}: {}", error.error, error.error_description))
        .unwrap_or_else(|_| "no OAuth error body".to_string())
}

fn http_client(redirects: Policy) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
        .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECONDS))
        .redirect(redirects)
        .build()
        .context("failed to build the gateway HTTP client")
}

async fn fetch(request: reqwest::RequestBuilder) -> Result<Reply> {
    let response = request.send().await.context("the gateway request failed")?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .context("failed to read the gateway response")?;
    Ok(Reply { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, thread};

    const NOW: i64 = 1_800_000_000;
    const GATEWAY: &str = "https://gateway.example.com";

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Discover(String),
        Register(String),
        Token(Grant),
    }

    struct FakeServer {
        discovery: Option<Discovery>,
        registered_client_id: Option<String>,
        token_replies: RefCell<VecDeque<Result<TokenReply>>>,
        calls: RefCell<Vec<Call>>,
    }

    impl FakeServer {
        fn supported() -> Self {
            Self {
                discovery: Some(Discovery::Supported(document())),
                registered_client_id: None,
                token_replies: RefCell::new(VecDeque::new()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn unreachable() -> Self {
            Self {
                discovery: None,
                ..Self::supported()
            }
        }

        fn registering(self, client_id: &str) -> Self {
            Self {
                registered_client_id: Some(client_id.to_string()),
                ..self
            }
        }

        fn replying(self, replies: Vec<Result<TokenReply>>) -> Self {
            Self {
                token_replies: RefCell::new(replies.into()),
                ..self
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }
    }

    impl AuthorizationServer for FakeServer {
        fn discover(&self, gateway_url: &str) -> Result<Discovery> {
            self.calls
                .borrow_mut()
                .push(Call::Discover(gateway_url.to_string()));
            self.discovery
                .clone()
                .ok_or_else(|| anyhow!("connection refused"))
        }

        fn register(&self, registration_endpoint: &str) -> Result<String> {
            self.calls
                .borrow_mut()
                .push(Call::Register(registration_endpoint.to_string()));
            self.registered_client_id
                .clone()
                .ok_or_else(|| anyhow!("registration was not expected"))
        }

        fn token(&self, _: &AuthorizationServerDocument, grant: Grant) -> Result<TokenReply> {
            self.calls.borrow_mut().push(Call::Token(grant));
            self.token_replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("no token reply scripted")))
        }
    }

    fn document() -> AuthorizationServerDocument {
        AuthorizationServerDocument {
            token_endpoint: format!("{GATEWAY}/token"),
            registration_endpoint: format!("{GATEWAY}/register"),
            resource: Some(GATEWAY.to_string()),
        }
    }

    fn issued(access_token: &str, refresh_token: &str) -> Result<TokenReply> {
        Ok(TokenReply::Issued(IssuedTokens {
            access_token: access_token.to_string(),
            refresh_token: Some(refresh_token.to_string()),
            expires_in: Some(86_400),
            user_id: Some("dev".to_string()),
            team_id: Some("team-a".to_string()),
        }))
    }

    fn refused(status: u16, error: &str) -> Result<TokenReply> {
        Ok(TokenReply::Refused {
            status,
            error: OAuthError {
                error: error.to_string(),
                error_description: format!("{error} description"),
            },
        })
    }

    fn cached(
        team: Option<&str>,
        expires_at: i64,
        refresh_token: Option<&str>,
    ) -> CachedCredential {
        CachedCredential {
            gateway_url: GATEWAY.to_string(),
            team: team.map(str::to_string),
            client_id: "llm_dcrc_cached".to_string(),
            access_token: "sk-cached".to_string(),
            refresh_token: refresh_token.map(str::to_string),
            expires_at,
            issued_at: NOW - 1,
            user_id: Some("dev".to_string()),
            team_id: team.map(str::to_string),
        }
    }

    fn request(team: Option<&'static str>) -> CredentialRequest<'static> {
        CredentialRequest {
            gateway_url: GATEWAY,
            team,
            renewal: Renewal::NearExpiry,
        }
    }

    fn no_idp_token() -> Result<String> {
        bail!("no signed-in identity on this device")
    }

    fn idp_token(calls: &RefCell<u32>) -> impl FnMut() -> Result<String> + '_ {
        move || {
            *calls.borrow_mut() += 1;
            Ok("idp-jwt".to_string())
        }
    }

    fn exchange_grant(client_id: &str, team: Option<&str>) -> Call {
        Call::Token(Grant::Exchange {
            client_id: client_id.to_string(),
            subject_token: "idp-jwt".to_string(),
            team: team.map(str::to_string),
        })
    }

    #[test]
    fn should_serve_a_fresh_cached_credential_without_touching_the_gateway_or_idp() {
        let server = FakeServer::supported();
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW + 7_200, Some("rt-1"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        assert_eq!(
            resolved,
            Resolved::Issued {
                credential: store[0].clone(),
                changed: false
            }
        );
        assert!(server.calls().is_empty());
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_refresh_a_stale_credential_with_the_rotating_refresh_token() {
        let server = FakeServer::supported().replying(vec![issued("sk-new", "rt-2")]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW + 300, Some("rt-1"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        let Resolved::Issued {
            credential,
            changed,
        } = resolved
        else {
            panic!("expected an issued credential");
        };
        assert!(changed);
        assert_eq!(credential.access_token, "sk-new");
        assert_eq!(credential.refresh_token.as_deref(), Some("rt-2"));
        assert_eq!(credential.client_id, "llm_dcrc_cached");
        assert_eq!(credential.expires_at, NOW + 86_400);
        assert_eq!(
            server.calls(),
            vec![
                Call::Discover(GATEWAY.to_string()),
                Call::Token(Grant::Refresh {
                    client_id: "llm_dcrc_cached".to_string(),
                    refresh_token: "rt-1".to_string(),
                }),
            ]
        );
        assert_eq!(*idp_calls.borrow(), 0, "a refresh must not touch the IdP");
    }

    #[test]
    fn should_keep_the_refresh_token_when_the_renewal_answer_omits_one() {
        let server = FakeServer::supported().replying(vec![Ok(TokenReply::Issued(IssuedTokens {
            access_token: "sk-new".to_string(),
            refresh_token: None,
            expires_in: Some(86_400),
            user_id: None,
            team_id: None,
        }))]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW + 300, Some("rt-1"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        let Resolved::Issued { credential, .. } = resolved else {
            panic!("expected an issued credential");
        };
        assert_eq!(credential.access_token, "sk-new");
        assert_eq!(credential.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(credential.user_id.as_deref(), Some("dev"));
        assert_eq!(credential.team_id.as_deref(), Some("team-a"));
        assert_eq!(credential.expires_at, NOW + 86_400);
    }

    #[test]
    fn should_assume_a_lifetime_when_the_gateway_states_none() {
        let without_lifetime = || {
            Ok(TokenReply::Issued(IssuedTokens {
                access_token: "sk-new".to_string(),
                refresh_token: Some("rt-2".to_string()),
                expires_in: None,
                user_id: Some("dev".to_string()),
                team_id: Some("team-a".to_string()),
            }))
        };
        let idp_calls = RefCell::new(0);

        let exchanged = resolve(
            &FakeServer::supported()
                .registering("llm_dcrc_fresh")
                .replying(vec![without_lifetime()]),
            &mut idp_token(&idp_calls),
            &[],
            request(Some("team-a")),
            NOW,
        )
        .unwrap();
        let renewed = resolve(
            &FakeServer::supported().replying(vec![without_lifetime()]),
            &mut idp_token(&idp_calls),
            &[cached(Some("team-a"), NOW + 300, Some("rt-1"))],
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        for resolved in [exchanged, renewed] {
            let Resolved::Issued { credential, .. } = resolved else {
                panic!("expected an issued credential");
            };
            assert_eq!(
                credential.expires_at,
                NOW + ASSUMED_CREDENTIAL_LIFETIME_SECONDS
            );
            assert!(credential.is_fresh(NOW, Renewal::NearExpiry));
            assert!(!credential.is_fresh(
                NOW + ASSUMED_CREDENTIAL_LIFETIME_SECONDS - CREDENTIAL_REFRESH_SKEW_SECONDS,
                Renewal::NearExpiry
            ));
            assert!(!credential.is_valid(NOW + ASSUMED_CREDENTIAL_LIFETIME_SECONDS));
        }
    }

    #[test]
    fn should_re_exchange_the_identity_token_when_the_refresh_token_lapsed() {
        let server = FakeServer::supported().replying(vec![
            refused(400, "invalid_grant"),
            issued("sk-new", "rt-2"),
        ]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW - 10, Some("rt-old"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        let Resolved::Issued { credential, .. } = resolved else {
            panic!("expected an issued credential");
        };
        assert_eq!(credential.access_token, "sk-new");
        assert_eq!(
            server.calls()[2],
            exchange_grant("llm_dcrc_cached", Some("team-a"))
        );
        assert!(
            !server
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Register(_))),
            "a lapsed refresh token keeps the registration"
        );
        assert_eq!(*idp_calls.borrow(), 1);
    }

    #[test]
    fn should_re_register_when_the_gateway_forgot_the_client() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_fresh")
            .replying(vec![
                refused(400, "invalid_grant"),
                refused(401, "invalid_client"),
                issued("sk-new", "rt-2"),
            ]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW - 10, Some("rt-old"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        let Resolved::Issued { credential, .. } = resolved else {
            panic!("expected an issued credential");
        };
        assert_eq!(credential.client_id, "llm_dcrc_fresh");
        assert_eq!(
            server.calls()[2..],
            [
                exchange_grant("llm_dcrc_cached", Some("team-a")),
                Call::Register(format!("{GATEWAY}/register")),
                exchange_grant("llm_dcrc_fresh", Some("team-a")),
            ]
        );
    }

    #[test]
    fn should_exchange_again_when_the_refresh_gets_an_answer_that_is_not_oauth() {
        let server = FakeServer::supported().replying(vec![
            Ok(TokenReply::Refused {
                status: 422,
                error: OAuthError::default(),
            }),
            issued("sk-new", "rt-2"),
        ]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW - 10, Some("rt-old"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        let Resolved::Issued { credential, .. } = resolved else {
            panic!("expected an issued credential");
        };
        assert_eq!(credential.access_token, "sk-new");
        assert_eq!(*idp_calls.borrow(), 1);
    }

    #[test]
    fn should_renew_a_credential_past_half_its_lifetime_under_half_life_renewal() {
        let server = FakeServer::supported().replying(vec![issued("sk-new", "rt-2")]);
        let idp_calls = RefCell::new(0);
        let store = vec![CachedCredential {
            issued_at: NOW - 50_000,
            ..cached(Some("team-a"), NOW + 30_000, Some("rt-1"))
        }];
        let half_life = CredentialRequest {
            renewal: Renewal::HalfLife,
            ..request(Some("team-a"))
        };

        let resolved =
            resolve(&server, &mut idp_token(&idp_calls), &store, half_life, NOW).unwrap();

        let Resolved::Issued {
            credential,
            changed,
        } = resolved
        else {
            panic!("expected an issued credential");
        };
        assert!(changed);
        assert_eq!(credential.access_token, "sk-new");
        assert_eq!(credential.expires_at, NOW + 86_400);
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_keep_a_credential_with_more_than_half_its_lifetime_under_half_life_renewal() {
        let server = FakeServer::supported().replying(vec![issued("sk-new", "rt-2")]);
        let idp_calls = RefCell::new(0);
        let store = vec![CachedCredential {
            issued_at: NOW - 10_000,
            ..cached(Some("team-a"), NOW + 70_000, Some("rt-1"))
        }];
        let half_life = CredentialRequest {
            renewal: Renewal::HalfLife,
            ..request(Some("team-a"))
        };

        let resolved =
            resolve(&server, &mut idp_token(&idp_calls), &store, half_life, NOW).unwrap();

        assert_eq!(
            resolved,
            Resolved::Issued {
                credential: store[0].clone(),
                changed: false
            }
        );
        assert!(server.calls().is_empty());
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_keep_a_valid_credential_when_no_identity_token_is_available_for_the_exchange() {
        let server = FakeServer::supported().replying(vec![refused(400, "invalid_grant")]);
        let store = vec![cached(Some("team-a"), NOW + 120, Some("rt-lapsed"))];

        let resolved = resolve(
            &server,
            &mut no_idp_token,
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        assert_eq!(
            resolved,
            Resolved::Issued {
                credential: store[0].clone(),
                changed: false
            }
        );
    }

    #[test]
    fn should_surface_the_missing_identity_once_the_credential_expired() {
        let server = FakeServer::supported().replying(vec![refused(400, "invalid_grant")]);
        let store = vec![cached(Some("team-a"), NOW - 1, Some("rt-lapsed"))];

        let error = resolve(
            &server,
            &mut no_idp_token,
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap_err();

        assert!(error.to_string().contains("no signed-in identity"));
    }

    #[test]
    fn should_register_and_exchange_on_first_use() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_first")
            .replying(vec![issued("sk-first", "rt-1")]);
        let idp_calls = RefCell::new(0);

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &[],
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        assert_eq!(
            resolved,
            Resolved::Issued {
                credential: CachedCredential {
                    gateway_url: GATEWAY.to_string(),
                    team: Some("team-a".to_string()),
                    client_id: "llm_dcrc_first".to_string(),
                    access_token: "sk-first".to_string(),
                    refresh_token: Some("rt-1".to_string()),
                    expires_at: NOW + 86_400,
                    issued_at: NOW,
                    user_id: Some("dev".to_string()),
                    team_id: Some("team-a".to_string()),
                },
                changed: true
            }
        );
        assert_eq!(
            server.calls(),
            vec![
                Call::Discover(GATEWAY.to_string()),
                Call::Register(format!("{GATEWAY}/register")),
                exchange_grant("llm_dcrc_first", Some("team-a")),
            ]
        );
    }

    #[test]
    fn should_retry_the_exchange_once_with_a_fresh_registration() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_fresh")
            .replying(vec![
                refused(401, "invalid_client"),
                issued("sk-new", "rt-1"),
            ]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(None, NOW - 10, None)];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(None),
            NOW,
        )
        .unwrap();

        let Resolved::Issued { credential, .. } = resolved else {
            panic!("expected an issued credential");
        };
        assert_eq!(credential.client_id, "llm_dcrc_fresh");
        assert_eq!(
            server.calls()[1..],
            [
                exchange_grant("llm_dcrc_cached", None),
                Call::Register(format!("{GATEWAY}/register")),
                exchange_grant("llm_dcrc_fresh", None),
            ]
        );
        assert_eq!(*idp_calls.borrow(), 1, "the identity token is fetched once");
    }

    #[test]
    fn should_report_unsupported_without_signing_in_when_the_gateway_has_no_authorization_server() {
        let server = FakeServer {
            discovery: Some(Discovery::Unsupported),
            ..FakeServer::supported()
        };
        let idp_calls = RefCell::new(0);

        let resolved =
            resolve(&server, &mut idp_token(&idp_calls), &[], request(None), NOW).unwrap();

        assert_eq!(resolved, Resolved::Unsupported);
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_keep_one_credential_per_team() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_b")
            .replying(vec![issued("sk-b", "rt-b")]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW + 7_200, Some("rt-a"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-b")),
            NOW,
        )
        .unwrap();

        let Resolved::Issued { credential, .. } = resolved else {
            panic!("expected an issued credential");
        };
        assert_eq!(
            server.calls()[2],
            exchange_grant("llm_dcrc_b", Some("team-b"))
        );
        let merged = upsert(&store, credential);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].access_token, "sk-cached");
        assert_eq!(merged[1].access_token, "sk-b");
    }

    #[test]
    fn should_replace_the_credential_for_the_same_gateway_and_team() {
        let store = vec![cached(Some("team-a"), NOW, Some("rt-1"))];
        let renewed = CachedCredential {
            access_token: "sk-renewed".to_string(),
            ..cached(Some("team-a"), NOW + 86_400, Some("rt-2"))
        };

        let merged = upsert(&store, renewed);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].access_token, "sk-renewed");
    }

    #[test]
    fn should_serve_a_still_valid_credential_when_the_gateway_is_unreachable() {
        let server = FakeServer::supported().replying(vec![Err(anyhow!("connection refused"))]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW + 120, Some("rt-1"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        assert_eq!(
            resolved,
            Resolved::Issued {
                credential: store[0].clone(),
                changed: false
            }
        );
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_serve_a_still_valid_credential_when_discovery_is_unreachable() {
        let server = FakeServer::unreachable();
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW + 120, Some("rt-1"))];

        let resolved = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap();

        assert_eq!(
            resolved,
            Resolved::Issued {
                credential: store[0].clone(),
                changed: false
            }
        );
        assert_eq!(server.calls(), vec![Call::Discover(GATEWAY.to_string())]);
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_fail_when_discovery_is_unreachable_and_nothing_is_cached() {
        let server = FakeServer::unreachable();
        let idp_calls = RefCell::new(0);

        let error = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &[],
            request(Some("team-a")),
            NOW,
        )
        .unwrap_err();

        assert!(error.to_string().contains("connection refused"));
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_fail_when_the_gateway_is_unreachable_and_the_credential_expired() {
        let server = FakeServer::supported().replying(vec![Err(anyhow!("connection refused"))]);
        let idp_calls = RefCell::new(0);
        let store = vec![cached(Some("team-a"), NOW - 1, Some("rt-1"))];

        let error = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &store,
            request(Some("team-a")),
            NOW,
        )
        .unwrap_err();

        assert!(error.to_string().contains("connection refused"));
        assert_eq!(*idp_calls.borrow(), 0);
    }

    #[test]
    fn should_name_the_team_setting_when_an_exchange_without_a_team_is_refused() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_first")
            .replying(vec![refused(400, "invalid_grant")]);
        let idp_calls = RefCell::new(0);

        let error =
            resolve(&server, &mut idp_token(&idp_calls), &[], request(None), NOW).unwrap_err();

        assert!(error.to_string().contains("no team is set"));
        assert!(error.to_string().contains("--team <team>"));
    }

    #[test]
    fn should_name_the_configured_team_when_an_exchange_with_a_team_is_refused() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_first")
            .replying(vec![refused(400, "invalid_grant")]);
        let idp_calls = RefCell::new(0);

        let error = resolve(
            &server,
            &mut idp_token(&idp_calls),
            &[],
            request(Some("team-a")),
            NOW,
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid_grant description"));
        assert!(error.to_string().contains("set to team `team-a`"));
        assert!(!error.to_string().contains("no team is set"));
    }

    #[test]
    fn should_surface_the_gateway_refusal_on_exchange() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_first")
            .replying(vec![refused(400, "invalid_request")]);
        let idp_calls = RefCell::new(0);

        let error =
            resolve(&server, &mut idp_token(&idp_calls), &[], request(None), NOW).unwrap_err();

        assert!(error
            .to_string()
            .contains("invalid_request: invalid_request description"));
    }

    #[test]
    fn should_keep_a_refused_exchange_closed_for_the_helpers() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_first")
            .replying(vec![refused(400, "invalid_grant")]);
        let idp_calls = RefCell::new(0);
        let resolved = resolve(&server, &mut idp_token(&idp_calls), &[], request(None), NOW)
            .map(|_| unreachable!("the refusal is an error"));
        let fallback_calls = RefCell::new(0);

        let error = helper_bearer(GATEWAY, resolved, || {
            *fallback_calls.borrow_mut() += 1;
            Ok("id-token".to_string())
        })
        .unwrap_err();

        assert!(error.is::<ExchangeRefused>());
        assert!(error
            .to_string()
            .contains("the gateway refused the token exchange (HTTP 400 invalid_grant"));
        assert_eq!(*fallback_calls.borrow(), 0);
    }

    #[test]
    fn should_keep_a_failed_sign_in_closed_for_the_helpers() {
        let server = FakeServer::supported().registering("llm_dcrc_first");
        let resolved = resolve(&server, &mut no_idp_token, &[], request(None), NOW)
            .map(|_| unreachable!("the sign-in failure is an error"));
        let fallback_calls = RefCell::new(0);

        let error = helper_bearer(GATEWAY, resolved, || {
            *fallback_calls.borrow_mut() += 1;
            Ok("id-token".to_string())
        })
        .unwrap_err();

        assert!(error.is::<SignInFailed>());
        assert_eq!(error.to_string(), "no signed-in identity on this device");
        assert_eq!(*fallback_calls.borrow(), 0);
    }

    #[test]
    fn should_send_the_identity_token_when_discovery_fails() {
        let idp_calls = RefCell::new(0);
        let resolved = resolve(
            &FakeServer::unreachable(),
            &mut idp_token(&idp_calls),
            &[],
            request(Some("team-a")),
            NOW,
        )
        .map(|_| unreachable!("an unreachable gateway is an error"));

        let bearer = helper_bearer(GATEWAY, resolved, || Ok("id-token".to_string())).unwrap();

        let HelperBearer::IdentityToken { token, notice } = bearer else {
            panic!("expected the identity token, got {bearer:?}");
        };
        assert_eq!(token, "id-token");
        assert!(notice.starts_with(
            "gateway https://gateway.example.com could not issue a credential, sending the \
             identity token instead: "
        ));
        assert!(notice.ends_with("connection refused"));
    }

    #[test]
    fn should_send_the_identity_token_when_the_token_endpoint_breaks() {
        let server = FakeServer::supported()
            .registering("llm_dcrc_first")
            .replying(vec![Err(anyhow!(
                "the gateway answered HTTP 503 on the token endpoint"
            ))]);
        let idp_calls = RefCell::new(0);
        let resolved = resolve(&server, &mut idp_token(&idp_calls), &[], request(None), NOW)
            .map(|_| unreachable!("a broken token endpoint is an error"));

        let bearer = helper_bearer(GATEWAY, resolved, || Ok("id-token".to_string())).unwrap();

        assert!(
            matches!(bearer, HelperBearer::IdentityToken { ref token, ref notice }
            if token == "id-token" && notice.ends_with("HTTP 503 on the token endpoint"))
        );
    }

    #[test]
    fn should_send_the_identity_token_when_the_gateway_offers_no_exchange() {
        let bearer = helper_bearer(GATEWAY, Ok(GatewayCredential::Unsupported), || {
            Ok("id-token".to_string())
        })
        .unwrap();

        assert_eq!(
            bearer,
            HelperBearer::IdentityToken {
                token: "id-token".to_string(),
                notice: "gateway https://gateway.example.com offers no IdP token exchange; \
                         sending the identity token instead"
                    .to_string(),
            }
        );
    }

    #[test]
    fn should_surface_the_fallback_sign_in_failure() {
        let error = helper_bearer(GATEWAY, Err(anyhow!("connection refused")), || {
            Err(anyhow!("the sign-in timed out"))
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "the sign-in timed out");
    }

    #[test]
    fn should_print_the_issued_credential_as_is() {
        let bearer = helper_bearer(
            GATEWAY,
            Ok(GatewayCredential::Issued("sk-issued".to_string())),
            || unreachable!("an issued credential needs no identity token"),
        )
        .unwrap();

        assert_eq!(bearer, HelperBearer::Credential("sk-issued".to_string()));
    }

    #[test]
    fn should_treat_a_credential_inside_the_skew_window_as_stale() {
        let near_expiry = Renewal::NearExpiry;
        assert!(
            !cached(None, NOW + CREDENTIAL_REFRESH_SKEW_SECONDS, None).is_fresh(NOW, near_expiry)
        );
        assert!(
            cached(None, NOW + CREDENTIAL_REFRESH_SKEW_SECONDS + 1, None)
                .is_fresh(NOW, near_expiry)
        );
        let half_life = Renewal::HalfLife;
        let lifetime = |issued_at: i64, expires_at: i64| CachedCredential {
            issued_at,
            ..cached(None, expires_at, None)
        };
        assert!(lifetime(NOW - 10_000, NOW + 10_001).is_fresh(NOW, half_life));
        assert!(!lifetime(NOW - 10_000, NOW + 10_000).is_fresh(NOW, half_life));
        assert!(!lifetime(0, NOW + 80_000).is_fresh(NOW, half_life));
        assert!(cached(None, NOW + 1, None).is_valid(NOW));
        assert!(!cached(None, NOW, None).is_valid(NOW));
    }

    #[test]
    fn should_treat_an_unreadable_store_as_empty() {
        assert!(decode_store("{not json").credentials.is_empty());
        let store = CredentialStore {
            credentials: vec![cached(Some("team-a"), NOW, Some("rt-1"))],
        };
        let round_trip = decode_store(&serde_json::to_string(&store).unwrap());
        assert_eq!(round_trip.credentials, store.credentials);
    }

    #[test]
    fn should_parse_discovery_replies() {
        let unsupported_gateway = Reply {
            status: 404,
            body: r#"{"detail":"Not Found"}"#.to_string(),
        };
        assert_eq!(
            parse_discovery(unsupported_gateway, GATEWAY).unwrap(),
            Discovery::Unsupported
        );

        let without_exchange = Reply {
            status: 200,
            body: json!({
                "token_endpoint": format!("{GATEWAY}/token"),
                "registration_endpoint": format!("{GATEWAY}/register"),
                "grant_types_supported": ["authorization_code", "refresh_token"],
            })
            .to_string(),
        };
        assert_eq!(
            parse_discovery(without_exchange, GATEWAY).unwrap(),
            Discovery::Unsupported
        );

        let supported = Reply {
            status: 200,
            body: json!({
                "issuer": GATEWAY,
                "resource": GATEWAY,
                "token_endpoint": format!("{GATEWAY}/token"),
                "registration_endpoint": format!("{GATEWAY}/register"),
                "grant_types_supported": ["authorization_code", "refresh_token", TOKEN_EXCHANGE_GRANT],
            })
            .to_string(),
        };
        assert_eq!(
            parse_discovery(supported, GATEWAY).unwrap(),
            Discovery::Supported(document())
        );

        let broken = Reply {
            status: 503,
            body: String::new(),
        };
        assert!(parse_discovery(broken, GATEWAY).is_err());
    }

    #[test]
    fn should_keep_the_advertised_endpoints_on_the_configured_gateway_origin() {
        let advertised = |token_endpoint: &str, registration_endpoint: &str| Reply {
            status: 200,
            body: json!({
                "token_endpoint": token_endpoint,
                "registration_endpoint": registration_endpoint,
                "grant_types_supported": [TOKEN_EXCHANGE_GRANT],
            })
            .to_string(),
        };
        let endpoints = |reply: Reply, gateway_url: &str| {
            let Discovery::Supported(document) = parse_discovery(reply, gateway_url).unwrap()
            else {
                panic!("expected a supported gateway");
            };
            (document.token_endpoint, document.registration_endpoint)
        };

        let behind_a_tls_proxy = advertised(
            "http://gateway.example.com/token",
            "http://10.0.0.7:4000/register",
        );
        assert_eq!(
            endpoints(behind_a_tls_proxy, GATEWAY),
            (format!("{GATEWAY}/token"), format!("{GATEWAY}/register"))
        );

        let another_host = advertised(
            "https://attacker.example.net/collect?x=1",
            "https://attacker.example.net/register",
        );
        assert_eq!(
            endpoints(another_host, GATEWAY),
            (
                format!("{GATEWAY}/collect?x=1"),
                format!("{GATEWAY}/register")
            )
        );

        let under_a_root_path = advertised(
            "https://gateway.example.com/litellm/token",
            "https://gateway.example.com/litellm/register",
        );
        assert_eq!(
            endpoints(under_a_root_path, "https://gateway.example.com/litellm"),
            (
                "https://gateway.example.com/litellm/token".to_string(),
                "https://gateway.example.com/litellm/register".to_string()
            )
        );

        assert!(parse_discovery(advertised("/token", "/register"), GATEWAY).is_err());
    }

    #[test]
    fn should_parse_token_replies() {
        let issued_reply = Reply {
            status: 200,
            body: json!({
                "access_token": "sk-new",
                "token_type": "Bearer",
                "expires_in": 86400,
                "refresh_token": "rt-1",
                "user_id": "dev",
                "team_id": "team-a",
                "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
            })
            .to_string(),
        };
        assert_eq!(
            parse_token_reply(issued_reply).unwrap(),
            issued("sk-new", "rt-1").unwrap()
        );

        let refused_reply = Reply {
            status: 401,
            body:
                r#"{"error":"invalid_client","error_description":"unknown or malformed client_id"}"#
                    .to_string(),
        };
        assert_eq!(
            parse_token_reply(refused_reply).unwrap(),
            TokenReply::Refused {
                status: 401,
                error: OAuthError {
                    error: "invalid_client".to_string(),
                    error_description: "unknown or malformed client_id".to_string(),
                }
            }
        );

        let not_oauth = Reply {
            status: 404,
            body: r#"{"detail":"MCP server not found"}"#.to_string(),
        };
        assert_eq!(
            parse_token_reply(not_oauth).unwrap(),
            TokenReply::Refused {
                status: 404,
                error: OAuthError::default(),
            }
        );

        let redirected = Reply {
            status: 301,
            body: String::new(),
        };
        assert!(parse_token_reply(redirected)
            .unwrap_err()
            .to_string()
            .contains("redirected"));

        let unavailable = Reply {
            status: 503,
            body: r#"{"error":"temporarily_unavailable","error_description":"db"}"#.to_string(),
        };
        assert!(parse_token_reply(unavailable)
            .unwrap_err()
            .to_string()
            .contains("temporarily_unavailable"));
    }

    fn one_shot_gateway(response: &'static str) -> (String, thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let served = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                received.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&received);
                let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    let expected = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                    body.len() >= expected
                });
                if complete || read == 0 {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(received).unwrap()
        });
        (origin, served)
    }

    fn document_at(origin: &str) -> AuthorizationServerDocument {
        AuthorizationServerDocument {
            token_endpoint: format!("{origin}/token"),
            registration_endpoint: format!("{origin}/register"),
            resource: None,
        }
    }

    #[test]
    fn should_post_the_exchange_with_the_team_header_and_never_follow_a_redirect() {
        let (origin, served) = one_shot_gateway(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:9/elsewhere\r\n\
             content-length: 0\r\nconnection: close\r\n\r\n",
        );

        let error = HttpAuthorizationServer
            .token(
                &document_at(&origin),
                Grant::Exchange {
                    client_id: "llm_dcrc_1".to_string(),
                    subject_token: "idp-jwt".to_string(),
                    team: Some("team-a".to_string()),
                },
            )
            .unwrap_err();

        let sent = served.join().unwrap();
        assert!(error.to_string().contains("redirected"), "{error:#}");
        assert!(sent.starts_with("POST /token HTTP/1.1\r\n"), "{sent}");
        let headers = sent.to_ascii_lowercase();
        assert!(headers.contains("x-litellm-team-id: team-a\r\n"), "{sent}");
        assert!(
            headers.contains("content-type: application/x-www-form-urlencoded\r\n"),
            "{sent}"
        );
        assert!(
            sent.ends_with(
                "\r\n\r\ngrant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange\
                 &client_id=llm_dcrc_1&subject_token=idp-jwt\
                 &subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Ajwt"
            ),
            "{sent}"
        );
    }

    #[test]
    fn should_post_the_refresh_without_a_team_header_and_read_the_rotated_pair() {
        let (origin, served) = one_shot_gateway(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 67\r\n\
             connection: close\r\n\r\n\
             {\"access_token\":\"sk-new\",\"refresh_token\":\"rt-2\",\"expires_in\":86400}",
        );

        let reply = HttpAuthorizationServer
            .token(
                &document_at(&origin),
                Grant::Refresh {
                    client_id: "llm_dcrc_1".to_string(),
                    refresh_token: "rt-1".to_string(),
                },
            )
            .unwrap();

        let sent = served.join().unwrap();
        assert_eq!(
            reply,
            TokenReply::Issued(IssuedTokens {
                access_token: "sk-new".to_string(),
                refresh_token: Some("rt-2".to_string()),
                expires_in: Some(86_400),
                user_id: None,
                team_id: None,
            })
        );
        assert!(
            !sent.to_ascii_lowercase().contains("x-litellm-team-id"),
            "{sent}"
        );
        assert!(
            sent.ends_with(
                "\r\n\r\ngrant_type=refresh_token&client_id=llm_dcrc_1&refresh_token=rt-1"
            ),
            "{sent}"
        );
    }

    #[test]
    fn should_build_the_token_exchange_form_with_the_team_header() {
        let (form, team) = token_form(
            &document(),
            Grant::Exchange {
                client_id: "llm_dcrc_1".to_string(),
                subject_token: "idp-jwt".to_string(),
                team: Some("team-a".to_string()),
            },
        );

        assert_eq!(team.as_deref(), Some("team-a"));
        assert_eq!(
            form,
            vec![
                ("grant_type", TOKEN_EXCHANGE_GRANT.to_string()),
                ("client_id", "llm_dcrc_1".to_string()),
                ("subject_token", "idp-jwt".to_string()),
                ("subject_token_type", JWT_TOKEN_TYPE.to_string()),
                ("resource", GATEWAY.to_string()),
            ]
        );

        let (form, team) = token_form(
            &document(),
            Grant::Refresh {
                client_id: "llm_dcrc_1".to_string(),
                refresh_token: "rt-1".to_string(),
            },
        );
        assert_eq!(team, None);
        assert_eq!(
            form,
            vec![
                ("grant_type", "refresh_token".to_string()),
                ("client_id", "llm_dcrc_1".to_string()),
                ("refresh_token", "rt-1".to_string()),
                ("resource", GATEWAY.to_string()),
            ]
        );
    }
}
