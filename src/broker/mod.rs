//! The credential broker inside `relay serve`: it keeps the IdP session, the
//! exchanged Gateway session credential, and a short-lived minted team key in
//! process memory, and hands the key to allowed clients over a local socket.
//! Nothing here touches the on-disk token caches.

pub mod caller;
pub mod key;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod session;
#[cfg(unix)]
pub mod socket;
#[cfg(test)]
pub(crate) mod test_support;

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    ai_tools::{
        gateway_credential::{
            resolve, AuthorizationServer, CachedCredential, CredentialRequest,
            HttpAuthorizationServer, Renewal, Resolved, SignInFailed,
        },
        idp::display_name,
        token::{
            advance, Advanced, CachedSession, IdentityProvider, OidcProvider, SignIn,
            SignInRequired,
        },
    },
    auth::open_browser,
    config::{
        config_path, load_settings, relay_home, save_settings, GatewaySection, IdpSection,
        RelaySettings,
    },
    system::hostname,
};
use caller::{effective_callers, unanchored_callers, Peer, Verdict};
use key::{
    ExtendOutcome, HttpKeyServer, KeyServer, MintOutcome, MintRefusal, MintRequest,
    KEY_HALF_LIFE_SECONDS, KEY_LIFETIME_SECONDS,
};

pub const TICK: Duration = Duration::from_secs(60);
pub const SOCKET_FILE: &str = "broker.sock";
const PERMISSION_ROUTES: &str = "[\"/key/generate\", \"/key/update\", \"/key/delete\"]";

pub fn socket_path() -> PathBuf {
    relay_home().join(SOCKET_FILE)
}

pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsVersion(u64);

pub trait SettingsSource: Send + Sync {
    fn version(&self) -> Option<SettingsVersion>;
    fn load(&self) -> Option<RelaySettings>;
    fn store(&self, settings: &RelaySettings) -> Result<()>;
}

pub struct FileSettings;

impl SettingsSource for FileSettings {
    fn version(&self) -> Option<SettingsVersion> {
        let metadata = fs::metadata(config_path()).ok()?;
        let modified = metadata.modified().ok()?;
        let mut hasher = DefaultHasher::new();
        modified.hash(&mut hasher);
        metadata.len().hash(&mut hasher);
        Some(SettingsVersion(hasher.finish()))
    }

    fn load(&self) -> Option<RelaySettings> {
        match load_settings() {
            Ok(settings) => Some(settings),
            Err(error) => {
                eprintln!(
                    "broker: the Relay config could not be reloaded, keeping the current settings: {error:#}"
                );
                None
            }
        }
    }

    fn store(&self, settings: &RelaySettings) -> Result<()> {
        save_settings(settings).map(|_| ())
    }
}

pub trait Handler: Send + Sync {
    fn handle(&self, request: Request, peer: Peer) -> Reply;
}

pub trait CallerCheck: Send + Sync {
    fn check(&self, callers: &[caller::AllowedCaller], peer: Peer) -> Verdict;
}

#[cfg(target_os = "macos")]
pub struct SignatureCallerCheck;

#[cfg(target_os = "macos")]
impl CallerCheck for SignatureCallerCheck {
    fn check(&self, callers: &[caller::AllowedCaller], peer: Peer) -> Verdict {
        caller::walk(&macos::MacProcessTable, callers, peer)
    }
}

#[cfg(not(target_os = "macos"))]
pub struct UnsupportedCallerCheck;

#[cfg(not(target_os = "macos"))]
impl CallerCheck for UnsupportedCallerCheck {
    fn check(&self, _callers: &[caller::AllowedCaller], _peer: Peer) -> Verdict {
        Verdict::Unsupported
    }
}

fn announce_callers(callers: &[caller::AllowedCaller], unanchored: &[caller::AllowedCaller]) {
    eprintln!(
        "broker: serving credentials to [{}]",
        callers
            .iter()
            .map(caller::AllowedCaller::describe)
            .collect::<Vec<_>>()
            .join(", ")
    );
    for ignored in unanchored {
        eprintln!(
            "broker: allowed caller {} is ignored: a managed entry needs team_id, since an identifier alone is satisfied by any ad hoc signature",
            ignored.describe()
        );
    }
    for unusable in callers
        .iter()
        .filter(|caller| caller.requirement().is_none())
    {
        eprintln!(
            "broker: allowed caller {} can never match: an identifier or team id must not be empty or contain a quote, a backslash, or a control character",
            unusable.describe()
        );
    }
}

pub fn host_caller_check() -> Box<dyn CallerCheck> {
    #[cfg(target_os = "macos")]
    {
        Box::new(SignatureCallerCheck)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(UnsupportedCallerCheck)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    Interactive,
    NonInteractive,
}

impl Context {
    fn sign_in(self) -> SignIn {
        match self {
            Context::Interactive => SignIn::Allowed,
            Context::NonInteractive => SignIn::CachedOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Credential { context: Context },
    ProxyCredential { context: Context },
    SignIn,
    SignOut,
    Status,
    SwitchTeam { team: String },
    SwitchEnvironment { environment: String },
    Recheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    MintedKey,
    SessionCredential,
    IdentityToken,
    StaticKey,
    ProxyToken,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::MintedKey => "minted_key",
            Source::SessionCredential => "session_credential",
            Source::IdentityToken => "identity_token",
            Source::StaticKey => "static_key",
            Source::ProxyToken => "proxy_token",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    pub token: String,
    pub expires_at: Option<i64>,
    pub source: Source,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    CallerRefused(String),
    SignedOut(String),
    SignInFailed(String),
    GatewayError(String),
    BadRequest(String),
    UnknownEnvironment(String),
    UnknownTeam(String),
    SwitchFailed(String),
}

impl Refusal {
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::CallerRefused(_) => "caller_refused",
            Refusal::SignedOut(_) => "signed_out",
            Refusal::SignInFailed(_) => "sign_in_failed",
            Refusal::GatewayError(_) => "gateway_error",
            Refusal::BadRequest(_) => "bad_request",
            Refusal::UnknownEnvironment(_) => "unknown_environment",
            Refusal::UnknownTeam(_) => "unknown_team",
            Refusal::SwitchFailed(_) => "switch_failed",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Refusal::CallerRefused(message)
            | Refusal::SignedOut(message)
            | Refusal::SignInFailed(message)
            | Refusal::GatewayError(message)
            | Refusal::BadRequest(message)
            | Refusal::UnknownEnvironment(message)
            | Refusal::UnknownTeam(message)
            | Refusal::SwitchFailed(message) => message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBearer {
    pub token: String,
    pub gateway_url: String,
}

pub const PROXY_TOKEN_PREFIX: &str = "relay-proxy-";

fn new_proxy_token() -> String {
    format!(
        "{PROXY_TOKEN_PREFIX}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn same_bytes(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokerStatus {
    pub signed_in: bool,
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    pub team: Option<String>,
    pub environment: Option<String>,
    pub gateway_url: String,
    pub key_expires_at: Option<String>,
    pub key_extended_at: Option<String>,
    pub source: Option<&'static str>,
    pub refused_callers: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Switched {
    pub team: Option<String>,
    pub environment: Option<String>,
    pub gateway_url: String,
    pub key_expires_at: Option<String>,
    pub source: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Credential(Issued),
    SignedIn { user_id: Option<String> },
    SignedOut,
    Status(BrokerStatus),
    Switched(Switched),
    Account(Value),
    Refused(Refusal),
}

impl Reply {
    pub fn to_json(&self) -> Value {
        match self {
            Reply::Credential(issued) => json!({
                "ok": true,
                "token": issued.token,
                "expires_at": issued.expires_at.map(rfc3339),
                "source": issued.source.name(),
                "notice": issued.notice,
            }),
            Reply::SignedIn { user_id } => {
                json!({ "ok": true, "signed_in": true, "user_id": user_id })
            }
            Reply::SignedOut => json!({ "ok": true, "signed_in": false }),
            Reply::Status(status) => with_ok(serde_json::to_value(status)),
            Reply::Switched(switched) => with_ok(serde_json::to_value(switched)),
            Reply::Account(status) => with_ok(Ok(status.clone())),
            Reply::Refused(refusal) => json!({
                "ok": false,
                "reason": refusal.reason(),
                "message": refusal.message(),
            }),
        }
    }
}

fn with_ok(body: serde_json::Result<Value>) -> Value {
    match body {
        Ok(Value::Object(fields)) => Value::Object(
            fields
                .into_iter()
                .chain([("ok".to_string(), Value::Bool(true))])
                .collect(),
        ),
        _ => json!({ "ok": true }),
    }
}

pub fn rfc3339(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| timestamp.to_string())
}

#[derive(Clone, PartialEq, Eq)]
enum Mode {
    Idp(IdpSection),
    StaticKey(String),
    Unconfigured(String),
}

impl Mode {
    fn from_settings(settings: &RelaySettings) -> Self {
        if settings.idp.is_configured() {
            return Mode::Idp(IdpSection {
                authorize_url: String::new(),
                ..settings.idp.clone()
            });
        }
        let static_key = settings
            .gateway
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty());
        match static_key {
            Some(key) => Mode::StaticKey(key.to_string()),
            None => Mode::Unconfigured(format!(
                "no IdP or Gateway key configured on this device; {}",
                settings.idp.setup_hint()
            )),
        }
    }
}

#[derive(Clone)]
struct Target {
    version: Option<SettingsVersion>,
    mode: Mode,
    gateway_url: String,
    team: Option<String>,
    environment: Option<String>,
    callers: Vec<caller::AllowedCaller>,
    unanchored: Vec<caller::AllowedCaller>,
}

impl Target {
    fn from_settings(settings: &RelaySettings, version: Option<SettingsVersion>) -> Self {
        let managed = settings.credential.allowed_callers.as_deref();
        Self {
            version,
            mode: Mode::from_settings(settings),
            gateway_url: settings.gateway.url.trim_end_matches('/').to_string(),
            team: settings.managed_team(),
            environment: settings
                .current_environment()
                .map(|entry| entry.name.clone()),
            callers: effective_callers(managed),
            unanchored: unanchored_callers(managed),
        }
    }
}

enum GatewayPatch {
    Team(Option<String>),
    Environment { url: String, team: Option<String> },
}

impl GatewayPatch {
    fn applied_to(&self, settings: RelaySettings) -> RelaySettings {
        let gateway = match self {
            GatewayPatch::Team(team) => GatewaySection {
                team: team.clone(),
                ..settings.gateway
            },
            GatewayPatch::Environment { url, team } => GatewaySection {
                url: url.clone(),
                team: team.clone(),
                ..settings.gateway
            },
        };
        RelaySettings {
            gateway,
            ..settings
        }
    }
}

fn unknown_environment(name: &str, settings: &RelaySettings) -> String {
    let names = settings
        .environments
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    match names.is_empty() {
        true => format!("no environment named {name:?}; none are configured"),
        false => format!(
            "no environment named {name:?}; configured: {}",
            names.join(", ")
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MintedKey {
    token: String,
    alias: String,
    expires_at: i64,
    extended_at: Option<i64>,
    extension_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StickyRefusal {
    credential_issued_at: i64,
    notice: String,
}

#[derive(Default)]
struct State {
    session: Option<CachedSession>,
    credential: Option<CachedCredential>,
    key: Option<MintedKey>,
    mint_refusal: Option<StickyRefusal>,
    last_source: Option<Source>,
    refused_callers: u64,
    proxy_token: Option<String>,
}

enum Bearer {
    Credential(CachedCredential),
    IdentityToken { token: String, expires_at: i64 },
}

pub struct Dependencies {
    pub auth: Box<dyn AuthorizationServer + Send + Sync>,
    pub keys: Box<dyn KeyServer + Send + Sync>,
    pub identity: Box<dyn IdentityProvider + Send + Sync>,
    pub clock: Box<dyn Clock>,
    pub callers: Box<dyn CallerCheck>,
    pub browser: Box<dyn Fn(&str) + Send + Sync>,
    pub hostname: String,
    pub settings: Box<dyn SettingsSource>,
}

impl Dependencies {
    pub fn live() -> Self {
        Self {
            auth: Box::new(HttpAuthorizationServer),
            keys: Box::new(HttpKeyServer),
            identity: Box::new(OidcProvider),
            clock: Box::new(SystemClock),
            callers: host_caller_check(),
            browser: Box::new(open_browser),
            hostname: hostname(),
            settings: Box::new(FileSettings),
        }
    }
}

pub struct Broker {
    target: Mutex<Target>,
    deps: Dependencies,
    state: Mutex<State>,
    renewal: Mutex<()>,
}

impl Broker {
    pub fn new(settings: &RelaySettings, deps: Dependencies) -> Self {
        let version = deps.settings.version();
        let target = Target::from_settings(settings, version);
        announce_callers(&target.callers, &target.unanchored);
        Self {
            target: Mutex::new(target),
            deps,
            state: Mutex::new(State::default()),
            renewal: Mutex::new(()),
        }
    }

    pub fn handle(&self, request: Request, peer: Peer) -> Reply {
        match request {
            Request::Credential { context } => self.credential(context, peer),
            Request::ProxyCredential { context } => self.proxy_credential(context, peer),
            Request::SignIn => self.sign_in(),
            Request::SignOut => self.sign_out(),
            Request::Status => Reply::Status(self.status()),
            Request::SwitchTeam { team } => self.switch_team(&team),
            Request::SwitchEnvironment { environment } => self.switch_environment(&environment),
            Request::Recheck => Reply::Refused(Refusal::BadRequest(
                "recheck is answered by the daemon's account service, not the broker".to_string(),
            )),
        }
    }

    pub fn credential(&self, context: Context, peer: Peer) -> Reply {
        match self.admit(peer, "credential") {
            Ok(target) => self.issue(&target, context),
            Err(refusal) => Reply::Refused(refusal),
        }
    }

    pub fn proxy_credential(&self, context: Context, peer: Peer) -> Reply {
        let target = match self.admit(peer, "proxy token") {
            Ok(target) => target,
            Err(refusal) => return Reply::Refused(refusal),
        };
        match self.issue(&target, context) {
            Reply::Credential(issued) => Reply::Credential(Issued {
                token: self
                    .lock_state()
                    .proxy_token
                    .get_or_insert_with(new_proxy_token)
                    .clone(),
                expires_at: None,
                source: Source::ProxyToken,
                notice: issued.notice,
            }),
            other => other,
        }
    }

    pub fn bearer_for_proxy(&self, presented: &str) -> Result<ProxyBearer, Refusal> {
        self.refresh();
        let known = self
            .lock_state()
            .proxy_token
            .as_deref()
            .is_some_and(|token| same_bytes(token, presented));
        if !known {
            return Err(Refusal::CallerRefused(
                "this is not the proxy token of the running Relay daemon; get one with `relay credential --proxy`"
                    .to_string(),
            ));
        }
        let target = self.lock_target().clone();
        match self.issue(&target, Context::NonInteractive) {
            Reply::Credential(issued) => Ok(ProxyBearer {
                token: issued.token,
                gateway_url: target.gateway_url,
            }),
            Reply::Refused(refusal) => Err(refusal),
            _ => Err(Refusal::GatewayError(
                "the broker answered without a credential".to_string(),
            )),
        }
    }

    fn admit(&self, peer: Peer, what: &str) -> Result<Target, Refusal> {
        self.refresh();
        let target = self.lock_target().clone();
        let verdict = self.deps.callers.check(&target.callers, peer);
        if let Some(message) = verdict.refusal_message() {
            self.lock_state().refused_callers += 1;
            eprintln!("broker: {message} (peer pid {})", peer.pid);
            return Err(Refusal::CallerRefused(message));
        }
        if let Verdict::Allowed { caller, level } = &verdict {
            eprintln!(
                "broker: {what} for {} ({level} above peer pid {})",
                caller.describe(),
                peer.pid
            );
        }
        Ok(target)
    }

    pub fn sign_in(&self) -> Reply {
        self.refresh();
        let _renewal = self.lock_renewal();
        let target = self.lock_target().clone();
        if !matches!(target.mode, Mode::Idp(_)) {
            return Reply::Refused(Refusal::GatewayError(
                "sign-in needs an IdP; this device serves the configured Gateway key".to_string(),
            ));
        }
        self.forget(&target.gateway_url, true);
        match self.issue_under_renewal_lock(&target, Context::Interactive) {
            Reply::Credential(_) => Reply::SignedIn {
                user_id: self
                    .lock_state()
                    .credential
                    .as_ref()
                    .and_then(|credential| credential.user_id.clone()),
            },
            other => other,
        }
    }

    pub fn sign_out(&self) -> Reply {
        let _renewal = self.lock_renewal();
        let gateway_url = self.lock_target().gateway_url.clone();
        self.forget(&gateway_url, true);
        Reply::SignedOut
    }

    pub fn switch_team(&self, team: &str) -> Reply {
        self.refresh();
        let settings = match self.switchable_settings() {
            Ok(settings) => settings,
            Err(refusal) => return Reply::Refused(refusal),
        };
        if settings.managed_team().as_deref() == Some(team) {
            return Reply::Switched(self.switched());
        }
        let previous = GatewayPatch::Team(settings.gateway.team.clone());
        self.apply_switch(
            settings,
            GatewayPatch::Team(Some(team.to_string())),
            previous,
        )
    }

    pub fn switch_environment(&self, name: &str) -> Reply {
        self.refresh();
        let settings = match self.switchable_settings() {
            Ok(settings) => settings,
            Err(refusal) => return Reply::Refused(refusal),
        };
        let Some(entry) = settings.environment_named(name) else {
            return Reply::Refused(Refusal::UnknownEnvironment(unknown_environment(
                name, &settings,
            )));
        };
        if settings
            .current_environment()
            .is_some_and(|current| current.name == entry.name)
        {
            return Reply::Switched(self.switched());
        }
        let next = GatewayPatch::Environment {
            url: entry.url.clone(),
            team: None,
        };
        let previous = GatewayPatch::Environment {
            url: settings.gateway.url.clone(),
            team: settings.gateway.team.clone(),
        };
        self.apply_switch(settings, next, previous)
    }

    fn switchable_settings(&self) -> Result<RelaySettings, Refusal> {
        let target = self.lock_target().clone();
        match &target.mode {
            Mode::Idp(_) => {}
            Mode::StaticKey(_) => {
                return Err(Refusal::GatewayError(
                    "switching needs an IdP; this device serves the configured Gateway key"
                        .to_string(),
                ))
            }
            Mode::Unconfigured(hint) => return Err(Refusal::GatewayError(hint.clone())),
        }
        self.deps
            .settings
            .load()
            .ok_or_else(|| Refusal::GatewayError("the Relay config could not be read".to_string()))
    }

    fn apply_switch(
        &self,
        settings: RelaySettings,
        next: GatewayPatch,
        previous: GatewayPatch,
    ) -> Reply {
        if let Err(error) = self.deps.settings.store(&next.applied_to(settings.clone())) {
            return Reply::Refused(Refusal::SwitchFailed(format!(
                "the Relay config could not be written: {error:#}"
            )));
        }
        self.refresh();
        let target = self.lock_target().clone();
        let refusal = match self.issue(&target, Context::Interactive) {
            Reply::Refused(refusal) => refusal,
            _ => return Reply::Switched(self.switched()),
        };
        eprintln!(
            "broker: the switch could not issue a credential ({}); restoring the previous config",
            refusal.message()
        );
        let restored = previous.applied_to(self.deps.settings.load().unwrap_or(settings));
        if let Err(error) = self.deps.settings.store(&restored) {
            eprintln!("broker: the previous config could not be restored: {error:#}");
        }
        self.refresh();
        Reply::Refused(Refusal::SwitchFailed(refusal.message().to_string()))
    }

    fn switched(&self) -> Switched {
        let status = self.status();
        Switched {
            team: status.team,
            environment: status.environment,
            gateway_url: status.gateway_url,
            key_expires_at: status.key_expires_at,
            source: status.source,
        }
    }

    pub fn shutdown(&self) {
        let _renewal = self.lock_renewal();
        let gateway_url = self.lock_target().gateway_url.clone();
        self.forget(&gateway_url, false);
    }

    pub fn status(&self) -> BrokerStatus {
        self.refresh();
        let target = self.lock_target().clone();
        let now = self.deps.clock.now();
        let state = self.lock_state();
        let credential = state.credential.as_ref();
        let signed_in = matches!(target.mode, Mode::StaticKey(_))
            || credential.is_some_and(|credential| credential.is_valid(now));
        BrokerStatus {
            signed_in,
            user_id: credential.and_then(|credential| credential.user_id.clone()),
            display_name: state
                .session
                .as_ref()
                .filter(|_| signed_in)
                .and_then(|session| display_name(&session.token)),
            team: target.team,
            environment: target.environment,
            gateway_url: target.gateway_url,
            key_expires_at: state.key.as_ref().map(|key| rfc3339(key.expires_at)),
            key_extended_at: state
                .key
                .as_ref()
                .and_then(|key| key.extended_at)
                .map(rfc3339),
            source: state.last_source.map(Source::name),
            refused_callers: state.refused_callers,
        }
    }

    /// Runs once a minute: renews the session credential at its half-life and
    /// extends the minted key at its own, both without a browser.
    pub fn tick(&self) {
        self.refresh();
        let _renewal = self.lock_renewal();
        let target = self.lock_target().clone();
        let Mode::Idp(idp) = &target.mode else {
            return;
        };
        let now = self.deps.clock.now();
        let Some(credential) = self.lock_state().credential.clone() else {
            return;
        };
        let bearer = match credential.is_fresh(now, Renewal::HalfLife) {
            true => credential,
            false => match self.session_credential(&target, idp, Context::NonInteractive, now) {
                Ok(Bearer::Credential(renewed)) => renewed,
                Ok(Bearer::IdentityToken { .. }) => return,
                Err(refusal) => {
                    eprintln!(
                        "broker: session credential renewal failed, keeping the current one: {}",
                        refusal.message()
                    );
                    credential
                }
            },
        };
        self.extend_key(&target, idp, &bearer, now);
    }

    fn refresh(&self) {
        let Some(version) = self.deps.settings.version() else {
            return;
        };
        if self.lock_target().version == Some(version) {
            return;
        }
        let Some(settings) = self.deps.settings.load() else {
            return;
        };
        let next = Target::from_settings(&settings, Some(version));
        let _renewal = self.lock_renewal();
        let current = self.lock_target().clone();
        if current.version == Some(version) {
            return;
        }
        if current.mode != next.mode {
            eprintln!("broker: the Relay config changed its IdP; signing out");
            self.forget(&current.gateway_url, true);
        } else if current.gateway_url != next.gateway_url {
            eprintln!(
                "broker: the Relay config moved the Gateway from {} to {}; the next request exchanges there with the same IdP session",
                current.gateway_url, next.gateway_url
            );
            self.forget(&current.gateway_url, false);
        } else if current.team != next.team {
            eprintln!(
                "broker: the Relay config changed the team from {} to {}; the next request exchanges again",
                current.team.as_deref().unwrap_or("none"),
                next.team.as_deref().unwrap_or("none")
            );
            self.forget(&current.gateway_url, false);
        }
        if current.callers != next.callers || current.unanchored != next.unanchored {
            announce_callers(&next.callers, &next.unanchored);
            self.lock_state().proxy_token = None;
        }
        *self.lock_target() = next;
    }

    fn issue(&self, target: &Target, context: Context) -> Reply {
        match &target.mode {
            Mode::Unconfigured(hint) => Reply::Refused(Refusal::GatewayError(hint.clone())),
            Mode::StaticKey(key) => self.serve(Issued {
                token: key.clone(),
                expires_at: None,
                source: Source::StaticKey,
                notice: None,
            }),
            Mode::Idp(_) => self.issue_from_idp(context),
        }
    }

    fn issue_from_idp(&self, context: Context) -> Reply {
        if let Some(issued) = self.fresh_key(self.deps.clock.now()) {
            return self.serve(issued);
        }
        let _renewal = self.lock_renewal();
        let target = self.lock_target().clone();
        self.issue_under_renewal_lock(&target, context)
    }

    fn issue_under_renewal_lock(&self, target: &Target, context: Context) -> Reply {
        let Mode::Idp(idp) = &target.mode else {
            return self.issue(target, context);
        };
        let now = self.deps.clock.now();
        if let Some(issued) = self.fresh_key(now) {
            return self.serve(issued);
        }
        let issued = match self.session_credential(target, idp, context, now) {
            Ok(Bearer::Credential(credential)) => {
                match self.key_or_credential(target, &credential, now) {
                    KeyOutcome::Issued(issued) => issued,
                    KeyOutcome::DeadBearer => {
                        return self.issue_after_dead_bearer(target, idp, context, &credential)
                    }
                }
            }
            Ok(Bearer::IdentityToken { token, expires_at }) => Issued {
                token,
                expires_at: Some(expires_at),
                source: Source::IdentityToken,
                notice: Some(format!(
                    "gateway {} offers no IdP token exchange; serving the identity token instead",
                    target.gateway_url
                )),
            },
            Err(refusal) => return Reply::Refused(refusal),
        };
        self.serve(issued)
    }

    fn issue_after_dead_bearer(
        &self,
        target: &Target,
        idp: &IdpSection,
        context: Context,
        dead: &CachedCredential,
    ) -> Reply {
        let now = self.deps.clock.now();
        self.mark_dead(dead, now);
        let issued = match self.session_credential(target, idp, context, now) {
            Ok(Bearer::Credential(credential)) => {
                match self.key_or_credential(target, &credential, now) {
                    KeyOutcome::Issued(issued) => issued,
                    KeyOutcome::DeadBearer => {
                        return Reply::Refused(Refusal::GatewayError(
                            "the gateway refused the freshly exchanged session credential"
                                .to_string(),
                        ))
                    }
                }
            }
            Ok(Bearer::IdentityToken { .. }) => {
                return Reply::Refused(Refusal::GatewayError(
                    "the gateway stopped offering the token exchange".to_string(),
                ))
            }
            Err(refusal) => return Reply::Refused(refusal),
        };
        self.serve(issued)
    }

    fn serve(&self, issued: Issued) -> Reply {
        self.lock_state().last_source = Some(issued.source);
        Reply::Credential(issued)
    }

    fn fresh_key(&self, now: i64) -> Option<Issued> {
        self.lock_state()
            .key
            .as_ref()
            .filter(|key| key.expires_at > now)
            .map(|key| Issued {
                token: key.token.clone(),
                expires_at: Some(key.expires_at),
                source: Source::MintedKey,
                notice: None,
            })
    }

    fn session_credential(
        &self,
        target: &Target,
        idp: &IdpSection,
        context: Context,
        now: i64,
    ) -> Result<Bearer, Refusal> {
        let sign_in = context.sign_in();
        let (mut session, store) = {
            let state = self.lock_state();
            (
                state.session.clone(),
                state.credential.clone().into_iter().collect::<Vec<_>>(),
            )
        };
        let request = CredentialRequest {
            gateway_url: &target.gateway_url,
            team: target.team.as_deref(),
            renewal: Renewal::HalfLife,
        };
        let resolved = {
            let mut identity_token = || self.identity_token(idp, &mut session, now, sign_in);
            resolve(
                self.deps.auth.as_ref(),
                &mut identity_token,
                &store,
                request,
                now,
            )
        };
        let unsupported = matches!(resolved, Ok(Resolved::Unsupported));
        let identity = unsupported.then(|| self.identity_token(idp, &mut session, now, sign_in));
        let session_expiry = session.as_ref().map(|session| session.exp);
        self.lock_state().session = session;
        match (resolved, identity) {
            (
                Ok(Resolved::Issued {
                    credential,
                    changed,
                }),
                _,
            ) => {
                if changed {
                    eprintln!(
                        "broker: gateway session credential issued for user {} (team {}), expires {}",
                        credential.user_id.as_deref().unwrap_or("unknown"),
                        credential.team_id.as_deref().unwrap_or("none"),
                        rfc3339(credential.expires_at)
                    );
                }
                self.lock_state().credential = Some(credential.clone());
                Ok(Bearer::Credential(credential))
            }
            (Ok(Resolved::Unsupported), Some(Ok(token))) => Ok(Bearer::IdentityToken {
                token,
                expires_at: session_expiry.unwrap_or(now),
            }),
            (Ok(Resolved::Unsupported), Some(Err(error))) | (Err(error), _) => Err(classify(error)),
            (Ok(Resolved::Unsupported), None) => Err(Refusal::GatewayError(
                "the gateway offers no token exchange".to_string(),
            )),
        }
    }

    fn identity_token(
        &self,
        idp: &IdpSection,
        session: &mut Option<CachedSession>,
        now: i64,
        sign_in: SignIn,
    ) -> Result<String> {
        let advanced = advance(
            self.deps.identity.as_ref(),
            idp,
            session.clone(),
            now,
            sign_in,
            self.deps.browser.as_ref(),
        )?;
        match advanced {
            Advanced::Reused(token) => Ok(token),
            Advanced::Renewed(renewed) => {
                eprintln!(
                    "broker: IdP session established, expires {}",
                    rfc3339(renewed.exp)
                );
                let token = renewed.token.clone();
                *session = Some(renewed);
                Ok(token)
            }
        }
    }

    fn key_or_credential(
        &self,
        target: &Target,
        credential: &CachedCredential,
        now: i64,
    ) -> KeyOutcome {
        let session_credential = |notice: Option<String>| {
            KeyOutcome::Issued(Issued {
                token: credential.access_token.clone(),
                expires_at: Some(credential.expires_at),
                source: Source::SessionCredential,
                notice,
            })
        };
        let Some(team) = target.team.as_deref() else {
            return session_credential(None);
        };
        let sticky = self
            .lock_state()
            .mint_refusal
            .clone()
            .filter(|refusal| refusal.credential_issued_at == credential.issued_at);
        if let Some(refusal) = sticky {
            return session_credential(Some(refusal.notice));
        }
        let request = MintRequest::new(team, &self.deps.hostname, now);
        let minted = self
            .deps
            .keys
            .mint(&target.gateway_url, &credential.access_token, &request);
        match minted {
            Ok(MintOutcome::Minted(minted)) => {
                let expires_at = minted.expires_at.unwrap_or(now + KEY_LIFETIME_SECONDS);
                eprintln!(
                    "broker: minted key {} for team {team}, expires {}",
                    request.alias,
                    rfc3339(expires_at)
                );
                let mut state = self.lock_state();
                state.key = Some(MintedKey {
                    token: minted.token.clone(),
                    alias: request.alias,
                    expires_at,
                    extended_at: None,
                    extension_failed: false,
                });
                state.mint_refusal = None;
                KeyOutcome::Issued(Issued {
                    token: minted.token,
                    expires_at: Some(expires_at),
                    source: Source::MintedKey,
                    notice: None,
                })
            }
            Ok(MintOutcome::Refused(MintRefusal::Unauthorized(message))) => {
                eprintln!(
                    "broker: the gateway no longer accepts the session credential ({message})"
                );
                KeyOutcome::DeadBearer
            }
            Ok(MintOutcome::Refused(refusal)) => {
                let notice = mint_notice(team, &refusal);
                eprintln!("broker: {notice}");
                if matches!(refusal, MintRefusal::TeamPermission(_)) {
                    self.lock_state().mint_refusal = Some(StickyRefusal {
                        credential_issued_at: credential.issued_at,
                        notice: notice.clone(),
                    });
                }
                session_credential(Some(notice))
            }
            Err(error) => {
                let notice = format!(
                    "key mint for team {team} failed ({error:#}); serving the session credential instead"
                );
                eprintln!("broker: {notice}");
                session_credential(Some(notice))
            }
        }
    }

    fn extend_key(&self, target: &Target, idp: &IdpSection, bearer: &CachedCredential, now: i64) {
        let due =
            self.lock_state().key.clone().filter(|key| {
                key.expires_at > now && key.expires_at - now <= KEY_HALF_LIFE_SECONDS
            });
        let Some(key) = due else {
            return;
        };
        let extended = self
            .deps
            .keys
            .extend(&target.gateway_url, &bearer.access_token, &key.token);
        match extended {
            Ok(ExtendOutcome::Extended { expires_at }) => {
                let expires_at = expires_at.unwrap_or(now + KEY_LIFETIME_SECONDS);
                let mut state = self.lock_state();
                if let Some(current) = state
                    .key
                    .as_mut()
                    .filter(|current| current.token == key.token)
                {
                    current.expires_at = expires_at;
                    current.extended_at = Some(now);
                    current.extension_failed = false;
                }
                eprintln!(
                    "broker: extended key {} to {}",
                    key.alias,
                    rfc3339(expires_at)
                );
            }
            Ok(ExtendOutcome::Unauthorized(message)) => {
                eprintln!(
                    "broker: the gateway no longer accepts the session credential ({message}); exchanging again"
                );
                self.mark_dead(bearer, now);
                if let Ok(Bearer::Credential(renewed)) =
                    self.session_credential(target, idp, Context::NonInteractive, now)
                {
                    let _ = self.key_or_credential(target, &renewed, now);
                }
            }
            Err(error) => {
                let mut state = self.lock_state();
                let Some(current) = state
                    .key
                    .as_mut()
                    .filter(|current| current.token == key.token)
                else {
                    return;
                };
                if !current.extension_failed {
                    eprintln!(
                        "broker: extending key {} failed, retrying next tick: {error:#}",
                        key.alias
                    );
                }
                current.extension_failed = true;
            }
        }
    }

    fn mark_dead(&self, credential: &CachedCredential, now: i64) {
        let mut state = self.lock_state();
        state.credential = Some(credential.dead(now));
        state.key = None;
        state.mint_refusal = None;
    }

    fn forget(&self, gateway_url: &str, clear_session: bool) {
        let (key, credential) = {
            let mut state = self.lock_state();
            let key = state.key.take();
            let credential = state.credential.take();
            state.mint_refusal = None;
            state.last_source = None;
            if clear_session {
                state.session = None;
                state.proxy_token = None;
            }
            (key, credential)
        };
        let (Some(key), Some(credential)) = (key, credential) else {
            return;
        };
        match self
            .deps
            .keys
            .delete(gateway_url, &credential.access_token, &key.token)
        {
            Ok(()) => eprintln!("broker: deleted key {}", key.alias),
            Err(error) => eprintln!(
                "broker: key {} not deleted ({error:#}); it expires at {}",
                key.alias,
                rfc3339(key.expires_at)
            ),
        }
    }

    fn lock_target(&self) -> MutexGuard<'_, Target> {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_renewal(&self) -> MutexGuard<'_, ()> {
        self.renewal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Handler for Broker {
    fn handle(&self, request: Request, peer: Peer) -> Reply {
        Broker::handle(self, request, peer)
    }
}

enum KeyOutcome {
    Issued(Issued),
    DeadBearer,
}

fn mint_notice(team: &str, refusal: &MintRefusal) -> String {
    match refusal {
        MintRefusal::TeamPermission(message) => format!(
            "the gateway refused to mint a key for team {team} ({message}); serving the session \
             credential instead. Admin fix: POST /team/permissions_update with team_id {team} and \
             team_member_permissions {PERMISSION_ROUTES}"
        ),
        MintRefusal::Unauthorized(message) | MintRefusal::Other(message) => format!(
            "key mint for team {team} failed ({message}); serving the session credential instead"
        ),
    }
}

fn classify(error: anyhow::Error) -> Refusal {
    match error.downcast_ref::<SignInFailed>() {
        Some(failed) if failed.cause().is::<SignInRequired>() => Refusal::SignedOut(
            "not signed in on this device; run `relay sign-in` or use the client interactively"
                .to_string(),
        ),
        Some(failed) => Refusal::SignInFailed(format!("{failed:#}")),
        None => Refusal::GatewayError(format!("{error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread, time::Duration};

    use super::{
        key::{ExtendOutcome, MintOutcome, MintRefusal},
        test_support::{
            idp_settings, static_key_settings, Rig, DISPLAY_NAME, GATEWAY, HOSTNAME, NOW,
        },
        *,
    };
    use crate::config::EnvironmentEntry;

    const MINUTE: i64 = 60;
    const UAT: &str = "https://uat.example.com";

    fn credential(rig: &Rig, context: Context) -> Reply {
        rig.broker
            .handle(Request::Credential { context }, rig.peer())
    }

    fn switch_team(rig: &Rig, team: &str) -> Reply {
        rig.broker.handle(
            Request::SwitchTeam {
                team: team.to_string(),
            },
            rig.peer(),
        )
    }

    fn switch_environment(rig: &Rig, name: &str) -> Reply {
        rig.broker.handle(
            Request::SwitchEnvironment {
                environment: name.to_string(),
            },
            rig.peer(),
        )
    }

    fn switched(reply: Reply) -> Switched {
        match reply {
            Reply::Switched(switched) => switched,
            other => panic!("expected a switch, got {other:?}"),
        }
    }

    fn environments_settings() -> RelaySettings {
        let mut settings = idp_settings(Some("team-a"));
        settings.environments = vec![
            EnvironmentEntry {
                name: "prod".to_string(),
                url: GATEWAY.to_string(),
                team: None,
            },
            EnvironmentEntry {
                name: "uat".to_string(),
                url: UAT.to_string(),
                team: Some("uat-team".to_string()),
            },
        ];
        settings
    }

    fn issued(reply: Reply) -> Issued {
        match reply {
            Reply::Credential(issued) => issued,
            other => panic!("expected a credential, got {other:?}"),
        }
    }

    fn refused(reply: Reply) -> Refusal {
        match reply {
            Reply::Refused(refusal) => refusal,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn other_idp_settings() -> RelaySettings {
        let mut settings = idp_settings(Some("team-a"));
        settings.idp.issuer = "https://other-idp.example.com".to_string();
        settings
    }

    fn other_gateway_settings() -> RelaySettings {
        let mut settings = idp_settings(Some("team-a"));
        settings.gateway.url = "https://other-gateway.example.com/".to_string();
        settings
    }

    #[test]
    fn should_sign_in_exchange_and_mint_a_team_key_on_the_first_request() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        let issued = issued(credential(&rig, Context::Interactive));
        assert_eq!(issued.token, "sk-1");
        assert_eq!(issued.source, Source::MintedKey);
        assert_eq!(issued.expires_at, Some(NOW + KEY_LIFETIME_SECONDS));
        assert_eq!(issued.notice, None);
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.auth.registrations(), 1);
        assert_eq!(rig.auth.exchanges(), 1);
        let mints = rig.keys.mints();
        assert_eq!(mints.len(), 1);
        assert_eq!(mints[0].bearer, "llm_session_1");
        assert_eq!(mints[0].team, "team-a");
        assert!(mints[0].alias.starts_with(&format!("relay-{HOSTNAME}-")));
        let status = rig.broker.status();
        assert!(status.signed_in);
        assert_eq!(status.user_id.as_deref(), Some("user-1"));
        assert_eq!(status.team.as_deref(), Some("team-a"));
        assert_eq!(
            status.key_expires_at,
            Some(rfc3339(NOW + KEY_LIFETIME_SECONDS))
        );
        assert_eq!(status.key_extended_at, None);
        assert_eq!(status.source, Some("minted_key"));
    }

    #[test]
    fn should_serve_the_same_key_and_extend_it_once_past_its_half_life() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.clock.advance(29 * MINUTE);
        rig.broker.tick();
        assert_eq!(rig.keys.extends(), Vec::<(String, String)>::new());
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        rig.clock.advance(2 * MINUTE);
        rig.broker.tick();
        assert_eq!(rig.auth.refreshes(), 1);
        assert_eq!(
            rig.keys.extends(),
            vec![("llm_session_2".to_string(), "sk-1".to_string())]
        );
        let issued_later = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued_later.token, "sk-1");
        assert_eq!(
            issued_later.expires_at,
            Some(NOW + 31 * MINUTE + KEY_LIFETIME_SECONDS)
        );
        let status = rig.broker.status();
        assert_eq!(status.key_extended_at, Some(rfc3339(NOW + 31 * MINUTE)));
        rig.clock.advance(28 * MINUTE);
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        assert_eq!(rig.keys.mints().len(), 1);
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_retry_a_failed_extension_on_the_next_tick() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        issued(credential(&rig, Context::Interactive));
        rig.keys
            .script_extend(Err(anyhow::anyhow!("connection reset")));
        rig.clock.advance(31 * MINUTE);
        rig.broker.tick();
        assert_eq!(rig.keys.extends().len(), 1);
        assert_eq!(rig.broker.status().key_extended_at, None);
        rig.clock.advance(MINUTE);
        rig.broker.tick();
        assert_eq!(rig.keys.extends().len(), 2);
        assert_eq!(
            rig.broker.status().key_extended_at,
            Some(rfc3339(NOW + 32 * MINUTE))
        );
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
    }

    #[test]
    fn should_replace_an_expired_key_with_a_fresh_mint_without_a_browser() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        issued(credential(&rig, Context::Interactive));
        rig.clock.advance(61 * MINUTE);
        let issued = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued.token, "sk-2");
        assert_eq!(issued.source, Source::MintedKey);
        assert_eq!(rig.keys.mints().len(), 2);
        assert_eq!(rig.keys.mints()[1].bearer, "llm_session_2");
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_fall_open_to_the_session_credential_when_the_team_cannot_mint_keys() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.keys
            .script_mint(Ok(MintOutcome::Refused(MintRefusal::TeamPermission(
                "team_member_permission denied".to_string(),
            ))));
        let issued_first = issued(credential(&rig, Context::Interactive));
        assert_eq!(issued_first.token, "llm_session_1");
        assert_eq!(issued_first.source, Source::SessionCredential);
        let notice = issued_first.notice.expect("a notice");
        assert!(notice.contains("/team/permissions_update"), "{notice}");
        assert!(notice.contains("/key/generate"), "{notice}");
        assert!(notice.contains("team-a"), "{notice}");
        let issued_again = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued_again.token, "llm_session_1");
        assert!(issued_again.notice.is_some());
        assert_eq!(rig.keys.mints().len(), 1);
        assert_eq!(rig.broker.status().source, Some("session_credential"));
        rig.clock.advance(31 * MINUTE);
        rig.broker.tick();
        let issued_renewed = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued_renewed.token, "sk-1");
        assert_eq!(rig.keys.mints().len(), 2);
        assert_eq!(rig.keys.mints()[1].bearer, "llm_session_2");
    }

    #[test]
    fn should_keep_serving_the_session_credential_on_a_transient_mint_failure() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.keys
            .script_mint(Rig::failing_mint("connection refused"));
        let issued_first = issued(credential(&rig, Context::Interactive));
        assert_eq!(issued_first.token, "llm_session_1");
        assert_eq!(issued_first.source, Source::SessionCredential);
        assert!(issued_first.notice.is_some());
        let issued_second = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued_second.token, "sk-1");
        assert_eq!(rig.keys.mints().len(), 2);
    }

    #[test]
    fn should_answer_signed_out_without_a_browser_when_nobody_is_watching() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        let refusal = refused(credential(&rig, Context::NonInteractive));
        assert_eq!(refusal.reason(), "signed_out");
        assert!(
            refusal.message().contains("relay sign-in"),
            "{}",
            refusal.message()
        );
        assert_eq!(rig.browser_opens(), 0);
        assert_eq!(rig.identity.sign_ins(), 0);
        assert_eq!(rig.keys.mints().len(), 0);
        assert!(!rig.broker.status().signed_in);
    }

    #[test]
    fn should_delete_the_key_and_forget_the_session_on_sign_out() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        issued(credential(&rig, Context::Interactive));
        assert_eq!(rig.broker.sign_out(), Reply::SignedOut);
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        assert!(!rig.broker.status().signed_in);
        assert_eq!(rig.broker.status().key_expires_at, None);
        assert_eq!(
            refused(credential(&rig, Context::NonInteractive)).reason(),
            "signed_out"
        );
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_delete_the_key_but_keep_the_idp_session_on_shutdown() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        issued(credential(&rig, Context::Interactive));
        rig.broker.shutdown();
        assert_eq!(rig.keys.deletes().len(), 1);
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-2"
        );
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_share_one_sign_in_between_concurrent_requests() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.identity.set_sign_in_delay(Duration::from_millis(300));
        let broker = Arc::clone(&rig.broker);
        let peer = rig.peer();
        let workers: Vec<_> = (0..3)
            .map(|_| {
                let broker = Arc::clone(&broker);
                thread::spawn(move || {
                    broker.handle(
                        Request::Credential {
                            context: Context::Interactive,
                        },
                        peer,
                    )
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(issued(worker.join().expect("worker")).token, "sk-1");
        }
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.keys.mints().len(), 1);
    }

    #[test]
    fn should_exchange_again_and_mint_anew_when_the_extension_answers_401() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.auth.set_session_lifetime(4 * 3600);
        issued(credential(&rig, Context::Interactive));
        rig.auth.rotate();
        rig.keys.script_extend(Ok(ExtendOutcome::Unauthorized(
            "LiteLLM Virtual Key expected".to_string(),
        )));
        rig.clock.advance(31 * MINUTE);
        rig.broker.tick();
        assert_eq!(rig.auth.registrations(), 2);
        assert_eq!(rig.keys.mints().len(), 2);
        assert_eq!(rig.keys.mints()[1].bearer, "llm_session_2");
        let issued = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued.token, "sk-2");
        assert_eq!(issued.source, Source::MintedKey);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.identity.sign_ins(), 1);
        let status = rig.broker.status();
        assert!(status.signed_in);
        assert_eq!(
            status.key_expires_at,
            Some(rfc3339(NOW + 31 * MINUTE + KEY_LIFETIME_SECONDS))
        );
    }

    #[test]
    fn should_exchange_again_and_mint_anew_when_the_mint_answers_401() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.auth.set_session_lifetime(4 * 3600);
        issued(credential(&rig, Context::Interactive));
        rig.clock.advance(61 * MINUTE);
        rig.auth.rotate();
        rig.keys
            .script_mint(Ok(MintOutcome::Refused(MintRefusal::Unauthorized(
                "LiteLLM Virtual Key expected".to_string(),
            ))));
        let issued = issued(credential(&rig, Context::Interactive));
        assert_eq!(issued.token, "sk-2");
        let bearers: Vec<String> = rig
            .keys
            .mints()
            .into_iter()
            .map(|call| call.bearer)
            .collect();
        assert_eq!(bearers, ["llm_session_1", "llm_session_1", "llm_session_2"]);
        assert_eq!(rig.auth.registrations(), 2);
        assert_eq!(rig.browser_opens(), 1);
        assert!(rig.broker.status().signed_in);
    }

    #[test]
    fn should_serve_the_configured_gateway_key_without_an_idp() {
        let rig = Rig::new(static_key_settings("sk-static"));
        let issued = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued.token, "sk-static");
        assert_eq!(issued.source, Source::StaticKey);
        assert_eq!(issued.expires_at, None);
        assert_eq!(rig.auth.exchanges(), 0);
        assert_eq!(rig.keys.mints().len(), 0);
        assert!(rig.broker.status().signed_in);
        assert_eq!(rig.broker.sign_in().to_json()["reason"], "gateway_error");
        rig.broker.tick();
        assert_eq!(rig.keys.extends().len(), 0);
    }

    #[test]
    fn should_refuse_when_neither_an_idp_nor_a_gateway_key_is_configured() {
        let rig = Rig::new(RelaySettings::default());
        let refusal = refused(credential(&rig, Context::Interactive));
        assert_eq!(refusal.reason(), "gateway_error");
        assert!(
            refusal.message().contains("--oidc-issuer"),
            "{}",
            refusal.message()
        );
        assert_eq!(rig.browser_opens(), 0);
    }

    #[test]
    fn should_serve_the_session_credential_itself_when_no_team_is_configured() {
        let rig = Rig::new(idp_settings(None));
        let issued = issued(credential(&rig, Context::Interactive));
        assert_eq!(issued.token, "llm_session_1");
        assert_eq!(issued.source, Source::SessionCredential);
        assert_eq!(issued.notice, None);
        assert_eq!(rig.keys.mints().len(), 0);
        assert_eq!(rig.broker.status().team, None);
    }

    #[test]
    fn should_count_a_refused_caller_and_never_touch_the_gateway() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.callers
            .refuse(&["zsh (unsigned)", "com.apple.Terminal (any team)"]);
        let refusal = refused(credential(&rig, Context::Interactive));
        assert_eq!(refusal.reason(), "caller_refused");
        assert!(
            refusal.message().contains("com.apple.Terminal"),
            "{}",
            refusal.message()
        );
        assert_eq!(rig.callers.checks(), 1);
        assert_eq!(rig.broker.status().refused_callers, 1);
        assert_eq!(rig.identity.sign_ins(), 0);
        assert_eq!(rig.auth.exchanges(), 0);
        assert_eq!(rig.keys.mints().len(), 0);
        assert_eq!(rig.browser_opens(), 0);
    }

    #[test]
    fn should_run_status_sign_in_and_sign_out_without_the_caller_check() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.callers.refuse(&["zsh (unsigned)"]);
        assert!(matches!(
            rig.broker.handle(Request::Status, rig.peer()),
            Reply::Status(_)
        ));
        assert!(matches!(
            rig.broker.handle(Request::SignIn, rig.peer()),
            Reply::SignedIn { .. }
        ));
        assert_eq!(
            rig.broker.handle(Request::SignOut, rig.peer()),
            Reply::SignedOut
        );
        assert_eq!(rig.callers.checks(), 0);
    }

    #[test]
    fn should_serve_the_identity_token_when_the_gateway_offers_no_exchange() {
        let rig = Rig::without_exchange(idp_settings(Some("team-a")));
        let first = issued(credential(&rig, Context::Interactive));
        assert_eq!(first.source, Source::IdentityToken);
        assert_eq!(first.token.matches('.').count(), 2);
        assert!(first
            .notice
            .expect("a notice")
            .contains("no IdP token exchange"));
        assert_eq!(rig.keys.mints().len(), 0);
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).source,
            Source::IdentityToken
        );
        assert_eq!(rig.identity.sign_ins(), 1);
    }

    #[test]
    fn should_force_a_fresh_sign_in_and_delete_the_old_key_on_sign_in() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        issued(credential(&rig, Context::Interactive));
        assert_eq!(
            rig.broker.sign_in(),
            Reply::SignedIn {
                user_id: Some("user-1".to_string())
            }
        );
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        assert_eq!(rig.identity.sign_ins(), 2);
        assert_eq!(rig.browser_opens(), 2);
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-2"
        );
    }

    #[test]
    fn should_mint_for_a_team_added_to_the_config_after_start_without_a_browser() {
        let rig = Rig::new(idp_settings(None));
        let first = issued(credential(&rig, Context::Interactive));
        assert_eq!(first.source, Source::SessionCredential);
        assert_eq!(rig.auth.exchanges(), 1);
        rig.settings.set(idp_settings(Some("team-a")));
        let second = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(second.source, Source::MintedKey);
        assert_eq!(second.token, "sk-1");
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.auth.exchanges(), 2);
        assert_eq!(rig.keys.mints()[0].team, "team-a");
        assert_eq!(rig.keys.deletes().len(), 0);
    }

    #[test]
    fn should_check_callers_against_the_allowlist_of_a_config_changed_after_start() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        assert_eq!(
            rig.callers.last_allowlist(),
            super::caller::default_callers()
        );
        let managed = vec![super::caller::AllowedCaller::new(
            "com.example.tool",
            Some("TEAM123456"),
        )];
        let mut changed = idp_settings(Some("team-a"));
        changed.credential.allowed_callers = Some(managed.clone());
        rig.settings.set(changed);
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        assert_eq!(rig.callers.last_allowlist(), managed);
        assert_eq!(rig.keys.deletes().len(), 0);
        assert_eq!(rig.identity.sign_ins(), 1);
    }

    #[test]
    fn should_keep_the_session_when_a_rewritten_config_only_drops_the_legacy_authorize_url() {
        let mut legacy = idp_settings(Some("team-a"));
        legacy.idp.authorize_url = "https://idp.example.com/authorize".to_string();
        let rig = Rig::new(legacy);
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.settings.set(idp_settings(Some("team-a")));
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        assert_eq!(rig.keys.deletes().len(), 0);
        assert_eq!(rig.keys.mints().len(), 1);
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_sign_out_when_the_config_changes_its_idp_and_sign_in_again_on_request() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.settings.set(other_idp_settings());
        assert_eq!(
            refused(credential(&rig, Context::NonInteractive)).reason(),
            "signed_out"
        );
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        assert_eq!(rig.browser_opens(), 1);
        let issued = issued(credential(&rig, Context::Interactive));
        assert_eq!(issued.token, "sk-2");
        assert_eq!(issued.source, Source::MintedKey);
        assert_eq!(rig.identity.sign_ins(), 2);
        assert_eq!(rig.browser_opens(), 2);
        assert_eq!(rig.keys.mints()[1].bearer, "llm_session_2");
    }

    #[test]
    fn should_delete_the_key_at_the_old_gateway_and_keep_the_idp_session_when_the_config_moves_the_gateway(
    ) {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.settings.set(other_gateway_settings());
        let moved = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(moved.token, "sk-2");
        assert_eq!(moved.source, Source::MintedKey);
        assert_eq!(rig.keys.delete_urls(), vec![GATEWAY.to_string()]);
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        assert_eq!(
            rig.keys.mints()[1].gateway_url,
            "https://other-gateway.example.com"
        );
        assert_eq!(rig.auth.exchanges(), 2);
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_keep_serving_when_a_changed_config_fails_to_load_and_apply_it_once_it_loads() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.settings.fail_next_load();
        rig.settings.set(idp_settings(Some("team-a")));
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        assert_eq!(rig.auth.exchanges(), 1);
        assert_eq!(rig.keys.deletes().len(), 0);
        rig.settings.fail_next_load();
        rig.settings.set(idp_settings(Some("team-b")));
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-1"
        );
        assert_eq!(rig.auth.exchanges(), 1);
        let moved = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(moved.token, "sk-2");
        assert_eq!(rig.auth.exchanges(), 2);
        assert_eq!(rig.keys.mints()[1].team, "team-b");
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_report_the_new_team_in_status_after_a_config_change() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.settings.set(idp_settings(Some("team-b")));
        let status = rig.broker.status();
        assert_eq!(status.team.as_deref(), Some("team-b"));
        assert_eq!(status.key_expires_at, None);
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        let issued = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(issued.token, "sk-2");
        assert_eq!(rig.keys.mints()[1].team, "team-b");
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-b"));
    }

    #[test]
    fn should_mint_for_the_config_that_landed_while_a_request_was_being_checked() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        let gate = rig.callers.gate_next_check();
        let broker = Arc::clone(&rig.broker);
        let peer = rig.peer();
        let waiting = thread::spawn(move || {
            broker.handle(
                Request::Credential {
                    context: Context::NonInteractive,
                },
                peer,
            )
        });
        gate.wait();
        rig.settings.set(idp_settings(Some("team-b")));
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-b"));
        gate.wait();
        let issued = issued(waiting.join().expect("worker"));
        assert_eq!(issued.token, "sk-2");
        let mints = rig.keys.mints();
        assert_eq!(mints.len(), 2);
        assert_eq!(mints[1].team, "team-b");
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
    }

    #[test]
    fn should_render_replies_as_one_json_object_each() {
        let credential = Reply::Credential(Issued {
            token: "sk-1".to_string(),
            expires_at: Some(NOW),
            source: Source::MintedKey,
            notice: None,
        })
        .to_json();
        assert_eq!(credential["ok"], true);
        assert_eq!(credential["token"], "sk-1");
        assert_eq!(credential["expires_at"], "2027-01-15T08:00:00Z");
        assert_eq!(credential["source"], "minted_key");
        let refused = Reply::Refused(Refusal::SignedOut("not signed in".to_string())).to_json();
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["reason"], "signed_out");
        assert_eq!(refused["message"], "not signed in");
        assert!(refused.get("token").is_none());
        let request: Request =
            serde_json::from_str("{\"op\":\"credential\",\"context\":\"non_interactive\"}")
                .expect("parse");
        assert_eq!(
            request,
            Request::Credential {
                context: Context::NonInteractive
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>("{\"op\":\"sign_out\"}").expect("parse"),
            Request::SignOut
        );
        assert_eq!(
            serde_json::from_str::<Request>("{\"op\":\"switch_team\",\"team\":\"team-b\"}")
                .expect("parse"),
            Request::SwitchTeam {
                team: "team-b".to_string()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(
                "{\"op\":\"switch_environment\",\"environment\":\"uat\"}"
            )
            .expect("parse"),
            Request::SwitchEnvironment {
                environment: "uat".to_string()
            }
        );
        let switched = Reply::Switched(Switched {
            team: Some("team-b".to_string()),
            environment: Some("uat".to_string()),
            gateway_url: UAT.to_string(),
            key_expires_at: Some(rfc3339(NOW)),
            source: Some("minted_key"),
        })
        .to_json();
        assert_eq!(switched["ok"], true);
        assert_eq!(switched["team"], "team-b");
        assert_eq!(switched["environment"], "uat");
        assert_eq!(switched["gateway_url"], UAT);
        assert_eq!(switched["key_expires_at"], "2027-01-15T08:00:00Z");
        assert_eq!(switched["source"], "minted_key");
        assert!(switched.get("token").is_none());
        assert_eq!(
            Reply::Refused(Refusal::UnknownEnvironment("no such".to_string())).to_json()["reason"],
            "unknown_environment"
        );
        assert_eq!(
            Reply::Refused(Refusal::SwitchFailed("refused".to_string())).to_json()["reason"],
            "switch_failed"
        );
        assert_eq!(
            Reply::Refused(Refusal::UnknownTeam("no such".to_string())).to_json()["reason"],
            "unknown_team"
        );
        assert_eq!(
            serde_json::from_str::<Request>("{\"op\":\"recheck\"}").expect("parse"),
            Request::Recheck
        );
        let account =
            Reply::Account(json!({"teams": [{"id": "team-a"}], "polled_at": null})).to_json();
        assert_eq!(account["ok"], true);
        assert_eq!(account["teams"], json!([{"id": "team-a"}]));
        assert_eq!(account["polled_at"], Value::Null);
        let rig = Rig::new(idp_settings(Some("team-a")));
        let bare = rig.broker.handle(Request::Recheck, rig.peer());
        assert!(
            matches!(bare, Reply::Refused(Refusal::BadRequest(_))),
            "{bare:?}"
        );
    }

    #[test]
    fn should_mint_under_the_new_team_and_delete_the_old_key_on_switch_team() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        let switched = switched(switch_team(&rig, "team-b"));
        assert_eq!(switched.team.as_deref(), Some("team-b"));
        assert_eq!(switched.environment, None);
        assert_eq!(switched.gateway_url, GATEWAY);
        assert_eq!(
            switched.key_expires_at,
            Some(rfc3339(NOW + KEY_LIFETIME_SECONDS))
        );
        assert_eq!(switched.source, Some("minted_key"));
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        let mints = rig.keys.mints();
        assert_eq!(mints.len(), 2);
        assert_eq!(mints[1].team, "team-b");
        assert_eq!(mints[1].bearer, "llm_session_2");
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(
            rig.settings
                .load()
                .expect("settings")
                .gateway
                .team
                .as_deref(),
            Some("team-b")
        );
        assert_eq!(
            issued(credential(&rig, Context::NonInteractive)).token,
            "sk-2"
        );
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-b"));
    }

    #[test]
    fn should_answer_switched_without_touching_the_gateway_when_the_team_is_already_current() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        let version = rig.settings.version();
        let switched = switched(switch_team(&rig, "team-a"));
        assert_eq!(switched.team.as_deref(), Some("team-a"));
        assert_eq!(switched.source, Some("minted_key"));
        assert_eq!(
            switched.key_expires_at,
            Some(rfc3339(NOW + KEY_LIFETIME_SECONDS))
        );
        assert_eq!(rig.settings.version(), version);
        assert_eq!(rig.keys.deletes().len(), 0);
        assert_eq!(rig.keys.mints().len(), 1);
        assert_eq!(rig.auth.exchanges(), 1);
    }

    #[test]
    fn should_revert_the_team_and_keep_serving_the_old_one_when_the_new_team_is_refused() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        rig.auth.refuse_team("team-b");
        let refusal = refused(switch_team(&rig, "team-b"));
        assert_eq!(refusal.reason(), "switch_failed");
        assert!(
            refusal.message().contains("access_denied"),
            "{}",
            refusal.message()
        );
        assert_eq!(rig.settings.load().expect("settings").gateway.team, None);
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        let again = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(again.token, "sk-2");
        assert_eq!(again.source, Source::MintedKey);
        assert_eq!(rig.keys.mints()[1].team, "team-a");
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-a"));
    }

    #[test]
    fn should_refuse_a_switch_without_an_idp_before_touching_the_config() {
        let rig = Rig::new(static_key_settings("sk-static"));
        let version = rig.settings.version();
        let refusal = refused(switch_team(&rig, "team-b"));
        assert_eq!(refusal.reason(), "gateway_error");
        assert!(
            refusal.message().contains("needs an IdP"),
            "{}",
            refusal.message()
        );
        assert_eq!(rig.settings.version(), version);
        assert_eq!(rig.callers.checks(), 0);
    }

    #[test]
    fn should_list_the_configured_names_when_the_environment_is_unknown() {
        let rig = Rig::new(environments_settings());
        let version = rig.settings.version();
        let refusal = refused(switch_environment(&rig, "staging"));
        assert_eq!(refusal.reason(), "unknown_environment");
        assert!(
            refusal.message().contains("\"staging\""),
            "{}",
            refusal.message()
        );
        assert!(
            refusal.message().contains("prod, uat"),
            "{}",
            refusal.message()
        );
        assert_eq!(rig.settings.version(), version);
        assert_eq!(rig.keys.mints().len(), 0);
        assert_eq!(rig.identity.sign_ins(), 0);
    }

    #[test]
    fn should_switch_environment_keeping_the_idp_session_and_moving_the_key() {
        let rig = Rig::new(environments_settings());
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        assert_eq!(rig.keys.mints()[0].team, "team-a");
        let moved = switched(switch_environment(&rig, "uat"));
        assert_eq!(moved.environment.as_deref(), Some("uat"));
        assert_eq!(moved.gateway_url, UAT);
        assert_eq!(moved.team.as_deref(), Some("uat-team"));
        assert_eq!(moved.source, Some("minted_key"));
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), 1);
        assert_eq!(rig.keys.delete_urls(), vec![GATEWAY.to_string()]);
        assert_eq!(
            rig.keys.deletes(),
            vec![("llm_session_1".to_string(), "sk-1".to_string())]
        );
        assert_eq!(rig.auth.exchanges(), 2);
        assert_eq!(rig.auth.discoveries().last().map(String::as_str), Some(UAT));
        let mints = rig.keys.mints();
        assert_eq!(mints.len(), 2);
        assert_eq!(mints[1].gateway_url, UAT);
        assert_eq!(mints[1].team, "uat-team");
        let stored = rig.settings.load().expect("settings");
        assert_eq!(stored.gateway.url, UAT);
        assert_eq!(stored.gateway.team, None);
        let again = switched(switch_environment(&rig, "uat"));
        assert_eq!(again.environment.as_deref(), Some("uat"));
        assert_eq!(rig.keys.mints().len(), 2);
        assert_eq!(rig.keys.deletes().len(), 1);
        let status = rig.broker.status();
        assert_eq!(status.environment.as_deref(), Some("uat"));
        assert_eq!(status.gateway_url, UAT);
        assert_eq!(status.team.as_deref(), Some("uat-team"));
    }

    #[test]
    fn should_revert_both_gateway_fields_when_the_environment_switch_is_refused() {
        let mut settings = environments_settings();
        settings.gateway.team = Some("picked-team".to_string());
        let rig = Rig::new(settings);
        assert_eq!(issued(credential(&rig, Context::Interactive)).token, "sk-1");
        assert_eq!(rig.keys.mints()[0].team, "picked-team");
        rig.auth.refuse_team("uat-team");
        let refusal = refused(switch_environment(&rig, "uat"));
        assert_eq!(refusal.reason(), "switch_failed");
        let stored = rig.settings.load().expect("settings");
        assert_eq!(stored.gateway.url, GATEWAY);
        assert_eq!(stored.gateway.team.as_deref(), Some("picked-team"));
        assert_eq!(rig.keys.delete_urls(), vec![GATEWAY.to_string()]);
        let again = issued(credential(&rig, Context::NonInteractive));
        assert_eq!(again.token, "sk-2");
        assert_eq!(rig.keys.mints()[1].gateway_url, GATEWAY);
        assert_eq!(rig.keys.mints()[1].team, "picked-team");
        assert_eq!(rig.identity.sign_ins(), 1);
        let status = rig.broker.status();
        assert_eq!(status.environment.as_deref(), Some("prod"));
        assert_eq!(status.gateway_url, GATEWAY);
    }

    #[test]
    fn should_report_the_display_name_from_the_id_token_only_while_signed_in() {
        let rig = Rig::new(environments_settings());
        let before = rig.broker.status();
        assert_eq!(before.display_name, None);
        assert_eq!(before.environment.as_deref(), Some("prod"));
        assert_eq!(before.gateway_url, GATEWAY);
        issued(credential(&rig, Context::Interactive));
        assert_eq!(
            rig.broker.status().display_name.as_deref(),
            Some(DISPLAY_NAME)
        );
        rig.broker.sign_out();
        assert_eq!(rig.broker.status().display_name, None);
    }

    fn proxy_token(rig: &Rig, context: Context) -> Reply {
        rig.broker
            .handle(Request::ProxyCredential { context }, rig.peer())
    }

    #[test]
    fn should_hand_out_one_proxy_token_that_is_never_the_gateway_credential() {
        let rig = Rig::new(idp_settings(Some("team-a")));

        let first = issued(proxy_token(&rig, Context::Interactive));
        let second = issued(proxy_token(&rig, Context::Interactive));

        assert_eq!(first.source, Source::ProxyToken);
        assert!(first.token.starts_with(PROXY_TOKEN_PREFIX));
        assert_eq!(first.token.len(), PROXY_TOKEN_PREFIX.len() + 64);
        assert_eq!(first.token, second.token);
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.keys.mints().len(), 1);
        assert_eq!(
            rig.broker.bearer_for_proxy(&first.token),
            Ok(ProxyBearer {
                token: "sk-1".to_string(),
                gateway_url: GATEWAY.trim_end_matches('/').to_string(),
            })
        );
    }

    #[test]
    fn should_drop_the_proxy_token_when_the_caller_allowlist_changes() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        let token = issued(proxy_token(&rig, Context::Interactive)).token;
        assert!(rig.broker.bearer_for_proxy(&token).is_ok());

        let mut changed = idp_settings(Some("team-a"));
        changed.credential.allowed_callers = Some(vec![super::caller::AllowedCaller::new(
            "com.example.tool",
            Some("TEAM123456"),
        )]);
        rig.settings.set(changed);

        assert_eq!(
            rig.broker.bearer_for_proxy(&token).unwrap_err().reason(),
            "caller_refused"
        );
        let reissued = issued(proxy_token(&rig, Context::NonInteractive)).token;
        assert_ne!(reissued, token);
        assert!(rig.broker.bearer_for_proxy(&reissued).is_ok());
        assert_eq!(rig.identity.sign_ins(), 1);
    }

    #[test]
    fn should_run_the_caller_check_before_handing_out_a_proxy_token() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        rig.callers.refuse(&["zsh (unsigned)"]);

        let refusal = refused(proxy_token(&rig, Context::Interactive));

        assert_eq!(refusal.reason(), "caller_refused");
        assert_eq!(rig.callers.checks(), 1);
        assert_eq!(rig.identity.sign_ins(), 0);
        assert_eq!(rig.broker.status().refused_callers, 1);
    }

    #[test]
    fn should_accept_only_the_issued_proxy_token_until_sign_out() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        assert_eq!(
            rig.broker
                .bearer_for_proxy("relay-proxy-guess")
                .unwrap_err()
                .reason(),
            "caller_refused"
        );
        let token = issued(proxy_token(&rig, Context::Interactive)).token;

        for wrong in ["", "sk-1", "relay-proxy-guess", &token[..token.len() - 1]] {
            assert_eq!(
                rig.broker.bearer_for_proxy(wrong).unwrap_err().reason(),
                "caller_refused",
                "{wrong:?} must not unlock the proxy"
            );
        }
        assert!(rig.broker.bearer_for_proxy(&token).is_ok());

        rig.broker.sign_out();

        assert_eq!(
            rig.broker.bearer_for_proxy(&token).unwrap_err().reason(),
            "caller_refused"
        );
        assert_eq!(rig.identity.sign_ins(), 1);
        let next = issued(proxy_token(&rig, Context::Interactive)).token;
        assert_ne!(next, token);
    }

    #[test]
    fn should_follow_a_replaced_key_for_proxied_requests_without_a_browser() {
        let rig = Rig::new(idp_settings(Some("team-a")));
        let token = issued(proxy_token(&rig, Context::Interactive)).token;
        let opens = rig.browser_opens();

        rig.clock.advance(61 * MINUTE);
        let bearer = rig.broker.bearer_for_proxy(&token).unwrap();

        assert_eq!(bearer.token, "sk-2");
        assert_eq!(rig.identity.sign_ins(), 1);
        assert_eq!(rig.browser_opens(), opens);
    }
}
